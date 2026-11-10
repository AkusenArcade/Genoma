// genoma:chart: a line, an area or bars from up to 1008 numbers, in one
// rectangle, no node per point.
// values: 0–3 the line's (or the bars') colour; 4–7 the fill under the
// line (alpha 0 for none); 8 the kind, 0 line and area, 1 bars; 9 how many
// points; 10, 11 the value at the bottom and at the top; 12 the line's
// width, logical units; 13 the gap between bars, 0…1 of a bar's slot;
// 14 the room kept above and below, logical units; 15 for an area, how
// much its fill fades toward the bottom (0…1), for bars their corner
// radius. The points from 16 on, oldest first.

fn chart_y(i: i32, h: f32) -> f32 {
    let lo = value(10);
    let hi = value(11);
    let inset = value(14);
    let v = clamp((value(16 + i) - lo) / max(hi - lo, 1e-6), 0.0, 1.0);
    return inset + (1.0 - v) * (h - 2.0 * inset);
}

fn chart_bars(p: vec2<f32>, n: i32) -> vec4<f32> {
    let w = shader.size.x;
    let h = shader.size.y;
    let slot = w / f32(n);
    let i = clamp(i32(floor(p.x / slot)), 0, n - 1);
    let gap = clamp(value(13), 0.0, 0.95) * slot;
    let top = chart_y(i, h);
    let bottom = h - value(14);
    let centre = vec2<f32>((f32(i) + 0.5) * slot, (top + bottom) * 0.5);
    let half = vec2<f32>(max(slot - gap, 0.0) * 0.5, max(bottom - top, 0.0) * 0.5);
    let r = min(value(15), min(half.x, half.y));
    let d = g_sd_rounded_box(p - centre, half, r);
    let c = g_rgba(0);
    return g_premul(vec4<f32>(c.rgb, c.a * g_cover(d)));
}

fn shade(p: vec2<f32>) -> vec4<f32> {
    let n = i32(value(9));
    if n < 1 {
        return vec4<f32>(0.0);
    }
    if value(8) > 0.5 {
        return chart_bars(p, n);
    }
    let w = shader.size.x;
    let h = shader.size.y;
    let half = value(12) * 0.5;
    if n == 1 {
        let y = chart_y(0, h);
        let c = g_rgba(0);
        return g_premul(vec4<f32>(c.rgb, c.a * g_cover(abs(p.y - y) - half)));
    }
    let dx = w / f32(n - 1);
    let fx = clamp(p.x / dx, 0.0, f32(n - 1));
    let i0 = min(i32(floor(fx)), n - 2);
    let t = fx - f32(i0);
    let line_y = mix(chart_y(i0, h), chart_y(i0 + 1, h), t);

    // The fill: under the line, fading toward the bottom as asked.
    let f = g_rgba(4);
    let below = max(h - line_y, 1e-3);
    let fade = 1.0 - clamp(value(15), 0.0, 1.0) * clamp((p.y - line_y) / below, 0.0, 1.0);
    let fill = g_premul(vec4<f32>(f.rgb, f.a * g_cover(line_y - p.y) * fade));

    // The line: the nearest of the segments a stroke this wide can reach.
    let reach = min(i32(ceil(half / dx)) + 1, 8);
    var d = 1e9;
    for (var j = max(i0 - reach, 0); j <= min(i0 + reach, n - 2); j = j + 1) {
        let a = vec2<f32>(f32(j) * dx, chart_y(j, h));
        let b = vec2<f32>(f32(j + 1) * dx, chart_y(j + 1, h));
        d = min(d, g_sd_segment(p, a, b));
    }
    let c = g_rgba(0);
    let line = g_premul(vec4<f32>(c.rgb, c.a * g_cover(d - half)));
    return g_over(line, fill);
}
