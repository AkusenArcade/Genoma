// genoma:boids: a flock. Each keeps apart from those too near, turns the
// way its neighbours go and drifts toward where they are; the pointer
// pulls them, or with a negative pull scatters them. They wrap round the
// node's edges. Genoma draws them (give the node an `agent_size`); the
// node itself shows nothing.
// values: 0 speed (logical units a second), 1 how far one sees, 2 how
// much it keeps apart, 3 how much it aligns, 4 how much it gathers, 5 the
// pointer's pull (negative scatters).
fn agent(i: i32, me: Agent) -> Agent {
    var a = me;
    let speed = max(value(0), 1.0);
    let n = f32(i);
    if (sim.frame == 0u) {
        let r = g_hash2(vec2<f32>(n * 0.731 + 0.5, n * 1.913 + 3.7));
        let angle = g_hash(vec2<f32>(n * 1.37 + 0.3, n * 0.21 + 3.1)) * 6.2831853;
        a.at = r * shader.size;
        a.velocity = vec2<f32>(cos(angle), sin(angle)) * speed;
        return a;
    }
    let sight = max(value(1), 1.0);
    var apart = vec2<f32>(0.0);
    var heading = vec2<f32>(0.0);
    var centre = vec2<f32>(0.0);
    var seen = 0.0;
    for (var j = 0; j < sim.agents; j = j + 1) {
        if (j == i) {
            continue;
        }
        let o = g_agent(j);
        // The nearest way round the wrapped edges.
        var d = o.at - me.at;
        d = d - shader.size * round(d / shader.size);
        let dist = length(d);
        if (dist < sight && dist > 1e-3) {
            // Pushed off those within half its sight, the nearer harder.
            apart = apart - d / dist * max(1.0 - dist / (sight * 0.5), 0.0);
            heading = heading + o.velocity;
            centre = centre + d;
            seen = seen + 1.0;
        }
    }
    // Accelerations, a second's worth each scaled by the step.
    var pull = vec2<f32>(0.0);
    if (seen > 0.0) {
        pull = pull + apart * value(2) * speed * 6.0;
        pull = pull + (heading / seen - me.velocity) * value(3) * 1.5;
        pull = pull + (centre / seen) * value(4) * 1.5;
    }
    if (shader.pointer.z > 0.5) {
        let d = shader.pointer.xy - me.at;
        pull = pull + d / max(length(d), 4.0) * value(5);
    }
    // A little wander of its own, so a flock never moves in lockstep.
    let wander = g_hash2(vec2<f32>(n * 0.37 + f32(sim.frame) * 0.013, n * 0.91)) - 0.5;
    pull = pull + wander * speed * 2.0;
    var v = me.velocity + pull * sim.dt;
    // Never stopped, never past twice its speed.
    let s = max(length(v), 1e-3);
    v = v / s * clamp(s, speed * 0.5, speed * 2.0);
    a.velocity = v;
    a.at = me.at + v * sim.dt;
    a.at = a.at - shader.size * floor(a.at / shader.size);
    return a;
}

fn shade(p: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(0.0);
}
