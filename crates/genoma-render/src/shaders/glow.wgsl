// genoma:glow, a pass (`pass = true`): what is bright in what it reads
// spread round it and added as light, a bloom.
// values: 0 how far it spreads, logical units; 1 its strength; 2 the
// brightness it starts from, 0…1.
fn shade(p: vec2<f32>) -> vec4<f32> {
    let seen = g_backdrop(p);
    let spread = max(value(0), 0.5);
    let threshold = clamp(value(2), 0.0, 1.0);
    // Taps on a sunflower spiral, turned by a different angle at every
    // pixel, weighed as a Gaussian: no copy of the shape stands out, the
    // few left over read as a fine grain.
    let turn = g_hash(floor(p * shader.scale)) * 6.2831853;
    var light = vec4<f32>(0.0);
    var weight = 0.0;
    let taps = 40;
    for (var i = 0; i < taps; i = i + 1) {
        let t = (f32(i) + 0.5) / f32(taps);
        let radius = sqrt(t) * spread;
        let angle = f32(i) * 2.3999632 + turn;
        let tap = g_backdrop(p + vec2<f32>(cos(angle), sin(angle)) * radius);
        let w = exp(-4.0 * t);
        // Premultiplied: what is past the threshold, as light.
        let bright = max(tap.rgb - vec3<f32>(threshold * tap.a), vec3<f32>(0.0));
        light = light + vec4<f32>(bright, max(bright.r, max(bright.g, bright.b))) * w;
        weight = weight + w;
    }
    light = light / weight * max(value(1), 0.0);
    return vec4<f32>(seen.rgb + light.rgb, seen.a + light.a * (1.0 - seen.a));
}
