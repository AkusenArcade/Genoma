// genoma:waves: water on a grid, the wave equation: the pointer stirs it
// as it moves over, and drops fall now and then. Lit from above-left, so
// the waves show as light and shade.
// values: 0 how fast waves travel (cells a step, below 0.5), 1 how much
// of a wave a step keeps (0.995), 2 drops a second, 3 a drop's width
// (cells); 4–7 the water's colour (straight RGBA); 8 the light, 0…1.
fn step(cell: vec2<i32>, me: vec4<f32>) -> vec4<f32> {
    if (sim.frame == 0u) {
        return vec4<f32>(0.0);
    }
    // x the height now, y a step before.
    let around = g_cell(cell + vec2<i32>(1, 0)).x + g_cell(cell - vec2<i32>(1, 0)).x
        + g_cell(cell + vec2<i32>(0, 1)).x + g_cell(cell - vec2<i32>(0, 1)).x;
    let c = clamp(value(0), 0.0, 0.5);
    var next = (2.0 * me.x - me.y + c * (around - 4.0 * me.x)) * clamp(value(1), 0.0, 1.0);
    let at = (vec2<f32>(cell) + 0.5) / vec2<f32>(sim.grid) * shader.size;
    // The pointer over it pushes the water down where it is.
    if (shader.pointer.z > 0.5) {
        let d = length(at - shader.pointer.xy);
        next = next - 0.6 * exp(-d * d / 40.0);
    }
    // A drop: once in a while, somewhere, as the rate says.
    let chance = value(2) * sim.dt;
    let tick = f32(sim.frame);
    let spot = g_hash2(vec2<f32>(tick * 0.137, tick * 0.731)) * vec2<f32>(sim.grid);
    if (g_hash(vec2<f32>(tick, 7.1)) < chance) {
        let d = length(vec2<f32>(cell) - spot);
        next = next + 1.5 * exp(-d * d / max(value(3) * value(3), 0.5));
    }
    return vec4<f32>(next, me.x, 0.0, 0.0);
}

fn shade(p: vec2<f32>) -> vec4<f32> {
    let cell = shader.size / vec2<f32>(max(g_grid(), vec2<i32>(1)));
    let dx = g_state(p + vec2<f32>(cell.x, 0.0)).x - g_state(p - vec2<f32>(cell.x, 0.0)).x;
    let dy = g_state(p + vec2<f32>(0.0, cell.y)).x - g_state(p - vec2<f32>(0.0, cell.y)).x;
    let normal = normalize(vec3<f32>(-dx, -dy, 0.6));
    let light = clamp(dot(normal, normalize(vec3<f32>(-0.5, -0.6, 0.7))), 0.0, 1.0);
    let water = g_rgba(4);
    let lit = water.rgb * (0.55 + 0.45 * light) + vec3<f32>(pow(light, 24.0)) * clamp(value(8), 0.0, 1.0);
    return g_premul(vec4<f32>(lit, water.a));
}
