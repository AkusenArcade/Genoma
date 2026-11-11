// genoma:reaction-diffusion: two chemicals on a grid (Gray-Scott), the
// second feeding on the first, spots and stripes growing out of a few
// seeds. Pressing the pointer over it drops more. Give it a grid of a
// cell every two logical units or so, and a few hundred steps a second.
// values: 0 feed (0.055), 1 kill (0.062), 2 and 3 how fast each spreads
// (1, 0.5); 4–7 the colour where there is none of the second, 8–11 where
// it is thickest (straight RGBA).
fn step(cell: vec2<i32>, me: vec4<f32>) -> vec4<f32> {
    if (sim.frame == 0u) {
        // Seeds: here and there a block of the second chemical.
        let block = vec2<f32>(cell / vec2<i32>(10));
        let seed = select(0.0, 1.0, g_hash(block + vec2<f32>(3.7, 1.3)) > 0.94);
        return vec4<f32>(1.0, seed, 0.0, 0.0);
    }
    var spread = -me;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            if (x == 0 && y == 0) {
                continue;
            }
            let w = select(0.2, 0.05, x != 0 && y != 0);
            spread = spread + g_cell(cell + vec2<i32>(x, y)) * w;
        }
    }
    let a = me.x;
    var b = me.y;
    let reaction = a * b * b;
    let feed = value(0);
    let kill = value(1);
    let na = a + value(2) * spread.x - reaction + feed * (1.0 - a);
    var nb = b + value(3) * spread.y + reaction - (kill + feed) * b;
    // The pointer, held, drops the second chemical where it is.
    let at = (vec2<f32>(cell) + 0.5) / vec2<f32>(sim.grid) * shader.size;
    if (shader.pointer.z > 0.5 && shader.pointer.w > 0.5 && length(at - shader.pointer.xy) < 6.0) {
        nb = 1.0;
    }
    return vec4<f32>(clamp(na, 0.0, 1.0), clamp(nb, 0.0, 1.0), 0.0, 0.0);
}

fn shade(p: vec2<f32>) -> vec4<f32> {
    let b = g_state(p).y;
    let t = smoothstep(0.08, 0.4, b);
    return g_premul(mix(g_rgba(4), g_rgba(8), t));
}
