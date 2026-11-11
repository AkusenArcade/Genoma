// genoma:pixelate, a pass (`pass = true`): what it reads in square cells,
// each the colour at its middle.
// values: 0 a cell's side, logical units.
fn shade(p: vec2<f32>) -> vec4<f32> {
    let cell = max(value(0), 1.0);
    return g_backdrop((floor(p / cell) + 0.5) * cell);
}
