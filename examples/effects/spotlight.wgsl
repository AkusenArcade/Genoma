// A light that follows the pointer over a field of noise: shader.pointer
// is where it is from the node's corner, z 1 while it is over the surface,
// w 1 while a button is held (the light opens wider).
fn shade(p: vec2<f32>) -> vec4<f32> {
    let base = 0.5 + 0.5 * g_fbm(p / 40.0, 4);
    let ground = vec3<f32>(0.06, 0.08, 0.12) + vec3<f32>(0.05, 0.07, 0.11) * base;
    var light = 0.0;
    if (shader.pointer.z > 0.5) {
        let reach = mix(45.0, 80.0, shader.pointer.w);
        light = exp(-pow(length(p - shader.pointer.xy) / reach, 2.0));
    }
    let lit = ground + vec3<f32>(0.53, 0.75, 0.82) * light * (0.6 + 0.4 * base);
    return vec4<f32>(lit, 1.0);
}
