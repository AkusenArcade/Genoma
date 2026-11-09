// genoma:grain: film grain, light and dark specks laid over what is under
// the node.
// values: 0 the amount, 0…1; 1 a speck's size, logical units; 2 how many
// times a second the grain changes (0 still; the node's clock must run).
fn shade(p: vec2<f32>) -> vec4<f32> {
    let size = max(value(1), 0.5);
    var frame = 0.0;
    if (value(2) > 0.0) {
        frame = floor(shader.time * value(2));
    }
    let v = (g_hash3(vec3<f32>(floor(p / size), frame)) * 2.0 - 1.0) * clamp(value(0), 0.0, 1.0);
    if (v > 0.0) {
        return vec4<f32>(vec3<f32>(v), v);
    }
    return vec4<f32>(0.0, 0.0, 0.0, -v);
}
