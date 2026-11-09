// genoma:noise: a field of drifting noise between two colours.
// values: 0–3 the colour where it is low, 4–7 where it is high (straight
// RGBA); 8 the size of a feature, logical units; 9 the drift, features a
// second (the node's clock must run); 10 the octaves; 11 the contrast.
fn shade(p: vec2<f32>) -> vec4<f32> {
    let size = max(value(8), 1.0);
    let n = g_fbm3(vec3<f32>(p / size, shader.time * value(9)), i32(value(10)));
    let t = clamp(0.5 + 0.5 * n * max(value(11), 0.0), 0.0, 1.0);
    return g_premul(mix(g_rgba(0), g_rgba(4), t));
}
