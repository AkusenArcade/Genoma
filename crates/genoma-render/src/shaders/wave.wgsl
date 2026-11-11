// genoma:wave, a pass (`pass = true`): what it reads bent by a travelling
// wave, rows sideways and columns, half as much, up and down.
// values: 0 how far, logical units; 1 the wavelength, logical units; 2 the
// speed, waves a second (the node's clock runs while it is above 0).
fn shade(p: vec2<f32>) -> vec4<f32> {
    let tau = 6.2831853;
    let wavelength = max(value(1), 1.0);
    let phase = shader.time * value(2) * tau;
    let bend = vec2<f32>(sin(p.y / wavelength * tau + phase), 0.5 * cos(p.x / wavelength * tau + phase)) * value(0);
    return g_backdrop(p + bend);
}
