// The sound as bars: value(0…63) are @genoma/spectrum's left channel,
// low to high, mirrored from the middle.
fn shade(p: vec2<f32>) -> vec4<f32> {
    let x = abs(p.x / shader.size.x - 0.5) * 2.0;
    let band = min(i32(x * 64.0), 63);
    let height = value(band) * shader.size.y;
    let from_bottom = shader.size.y - p.y;
    let lit = g_cover(from_bottom - height);
    let colour = mix(vec3<f32>(0.53, 0.75, 0.82), vec3<f32>(0.9, 0.69, 0.38), x);
    return vec4<f32>(colour * lit, lit);
}
