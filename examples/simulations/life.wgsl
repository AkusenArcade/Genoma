// A simulation of one's own: Conway's Game of Life, a cell alive (x = 1)
// or dead, born with three neighbours, living on with two or three. The
// grid starts at random; a press of the pointer seeds more. y keeps how
// recently a cell lived, for the shade to fade it out.
fn step(cell: vec2<i32>, me: vec4<f32>) -> vec4<f32> {
    if (sim.frame == 0u) {
        let alive = select(0.0, 1.0, g_hash(vec2<f32>(cell) * 0.731 + vec2<f32>(5.3, 1.9)) > 0.72);
        return vec4<f32>(alive, alive, 0.0, 0.0);
    }
    var around = 0.0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            if (x != 0 || y != 0) {
                around = around + g_cell(cell + vec2<i32>(x, y)).x;
            }
        }
    }
    var alive = select(0.0, 1.0, around == 3.0 || (me.x > 0.5 && around == 2.0));
    let at = (vec2<f32>(cell) + 0.5) / vec2<f32>(sim.grid) * shader.size;
    if (shader.pointer.w > 0.5 && length(at - shader.pointer.xy) < 10.0 && g_hash(at + vec2<f32>(f32(sim.frame))) > 0.5) {
        alive = 1.0;
    }
    return vec4<f32>(alive, max(alive, me.y * 0.85), 0.0, 0.0);
}

fn shade(p: vec2<f32>) -> vec4<f32> {
    let n = vec2<f32>(g_grid());
    let c = g_cell(vec2<i32>(floor(p / shader.size * n)));
    let glow = c.y;
    let colour = mix(vec3<f32>(0.05, 0.06, 0.09), vec3<f32>(0.73, 0.87, 0.42), glow);
    return vec4<f32>(colour, 1.0);
}
