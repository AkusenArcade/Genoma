// Genoma's shader library: in every g.shader, after the prelude, prefixed
// g_ so a shader's own names never meet them.
//
// Noise: g_hash, g_hash2, g_hash3 (0…1); g_value_noise (0…1); g_noise and
// g_noise3, gradient noise (-1…1, 3D for a field that moves with time);
// g_simplex (-1…1); g_worley, the distances to the nearest two cell points;
// g_fbm and g_fbm3, octaves of gradient noise (about -1…1).
// Fields: g_smin, g_smax; g_sd_circle, g_sd_box, g_sd_rounded_box,
// g_sd_segment, signed distances in the units given; g_cover, a distance
// in logical units as antialiased coverage.
// Colour: g_rgba(i), four bound values as a colour; g_premul; g_over.

fn g_hash(p: vec2<f32>) -> f32 {
    var q = fract(p * vec2<f32>(123.34, 456.21));
    q = q + dot(q, q + 45.32);
    return fract(q.x * q.y);
}

fn g_hash2(p: vec2<f32>) -> vec2<f32> {
    let a = g_hash(p);
    return vec2<f32>(a, g_hash(p + a + 17.13));
}

fn g_hash3(p: vec3<f32>) -> f32 {
    var q = fract(p * vec3<f32>(0.1031, 0.1030, 0.0973));
    q = q + dot(q, q.yxz + 33.33);
    return fract((q.x + q.y) * q.z);
}

fn g_fade(t: vec2<f32>) -> vec2<f32> {
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

// Smooth noise from a lattice of values, 0…1.
fn g_value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = g_fade(fract(p));
    let a = g_hash(i);
    let b = g_hash(i + vec2<f32>(1.0, 0.0));
    let c = g_hash(i + vec2<f32>(0.0, 1.0));
    let d = g_hash(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

fn g_gradient(i: vec2<f32>, f: vec2<f32>) -> f32 {
    let angle = g_hash(i) * 6.2831853;
    return dot(vec2<f32>(cos(angle), sin(angle)), f);
}

// Gradient (Perlin) noise, -1…1.
fn g_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = g_fade(f);
    let a = g_gradient(i, f);
    let b = g_gradient(i + vec2<f32>(1.0, 0.0), f - vec2<f32>(1.0, 0.0));
    let c = g_gradient(i + vec2<f32>(0.0, 1.0), f - vec2<f32>(0.0, 1.0));
    let d = g_gradient(i + vec2<f32>(1.0, 1.0), f - vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y) * 1.4142;
}

fn g_gradient3(i: vec3<f32>, f: vec3<f32>) -> f32 {
    let h = g_hash3(i) * 6.2831853;
    let z = g_hash3(i + 19.19) * 2.0 - 1.0;
    let r = sqrt(max(1.0 - z * z, 0.0));
    return dot(vec3<f32>(r * cos(h), r * sin(h), z), f);
}

// Gradient noise in three dimensions, -1…1: x and y a place, z the time.
fn g_noise3(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let n000 = g_gradient3(i, f);
    let n100 = g_gradient3(i + vec3<f32>(1.0, 0.0, 0.0), f - vec3<f32>(1.0, 0.0, 0.0));
    let n010 = g_gradient3(i + vec3<f32>(0.0, 1.0, 0.0), f - vec3<f32>(0.0, 1.0, 0.0));
    let n110 = g_gradient3(i + vec3<f32>(1.0, 1.0, 0.0), f - vec3<f32>(1.0, 1.0, 0.0));
    let n001 = g_gradient3(i + vec3<f32>(0.0, 0.0, 1.0), f - vec3<f32>(0.0, 0.0, 1.0));
    let n101 = g_gradient3(i + vec3<f32>(1.0, 0.0, 1.0), f - vec3<f32>(1.0, 0.0, 1.0));
    let n011 = g_gradient3(i + vec3<f32>(0.0, 1.0, 1.0), f - vec3<f32>(0.0, 1.0, 1.0));
    let n111 = g_gradient3(i + vec3<f32>(1.0, 1.0, 1.0), f - vec3<f32>(1.0, 1.0, 1.0));
    let x00 = mix(n000, n100, u.x);
    let x10 = mix(n010, n110, u.x);
    let x01 = mix(n001, n101, u.x);
    let x11 = mix(n011, n111, u.x);
    return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z) * 1.1547;
}

// Simplex noise in two dimensions, -1…1: fewer lattice points than g_noise,
// no square grain.
fn g_simplex(p: vec2<f32>) -> f32 {
    let k1 = 0.36602540; // (sqrt(3) - 1) / 2
    let k2 = 0.21132487; // (3 - sqrt(3)) / 6
    let i = floor(p + (p.x + p.y) * k1);
    let a = p - i + (i.x + i.y) * k2;
    let o = select(vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), a.x > a.y);
    let b = a - o + k2;
    let c = a - 1.0 + 2.0 * k2;
    let h = max(vec3<f32>(0.5) - vec3<f32>(dot(a, a), dot(b, b), dot(c, c)), vec3<f32>(0.0));
    let n = h * h * h * h * vec3<f32>(g_gradient(i, a), g_gradient(i + o, b), g_gradient(i + 1.0, c));
    return dot(n, vec3<f32>(70.0));
}

// Cellular noise: the distances to the nearest and second nearest of one
// point scattered in each unit cell.
fn g_worley(p: vec2<f32>) -> vec2<f32> {
    let i = floor(p);
    let f = fract(p);
    var d = vec2<f32>(8.0, 8.0);
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let cell = vec2<f32>(f32(x), f32(y));
            let r = length(cell + g_hash2(i + cell) - f);
            if (r < d.x) {
                d = vec2<f32>(r, d.x);
            } else if (r < d.y) {
                d.y = r;
            }
        }
    }
    return d;
}

// Octaves of gradient noise, each twice as fine and half as strong.
fn g_fbm(p: vec2<f32>, octaves: i32) -> f32 {
    var sum = 0.0;
    var amplitude = 0.5;
    var q = p;
    for (var o = 0; o < clamp(octaves, 1, 8); o = o + 1) {
        sum = sum + amplitude * g_noise(q);
        q = q * 2.03 + vec2<f32>(17.1, 9.7);
        amplitude = amplitude * 0.5;
    }
    return sum * 2.0;
}

fn g_fbm3(p: vec3<f32>, octaves: i32) -> f32 {
    var sum = 0.0;
    var amplitude = 0.5;
    var q = p;
    for (var o = 0; o < clamp(octaves, 1, 8); o = o + 1) {
        sum = sum + amplitude * g_noise3(q);
        q = vec3<f32>(q.xy * 2.03 + vec2<f32>(17.1, 9.7), q.z * 1.5);
        amplitude = amplitude * 0.5;
    }
    return sum * 2.0;
}

// Two fields joined, with a bridge as wide as k where they come close; and
// met where both are, rounded as wide as k.
fn g_smin(a: f32, b: f32, k: f32) -> f32 {
    let h = max(k - abs(a - b), 0.0) / max(k, 1e-5);
    return min(a, b) - h * h * k * 0.25;
}

fn g_smax(a: f32, b: f32, k: f32) -> f32 {
    return -g_smin(-a, -b, k);
}

fn g_sd_circle(p: vec2<f32>, r: f32) -> f32 {
    return length(p) - r;
}

// `p` from the box's centre; `half` its half size.
fn g_sd_box(p: vec2<f32>, half: vec2<f32>) -> f32 {
    let d = abs(p) - half;
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0);
}

fn g_sd_rounded_box(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let rr = min(r, min(half.x, half.y));
    return g_sd_box(p, half - rr) - rr;
}

fn g_sd_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0);
    return length(pa - ba * h);
}

// A distance in logical units as coverage, antialiased over one physical
// pixel: inside is 1.
fn g_cover(d: f32) -> f32 {
    return clamp(0.5 - d * shader.scale, 0.0, 1.0);
}

// Values i to i + 3 as a colour, straight (not premultiplied).
fn g_rgba(i: i32) -> vec4<f32> {
    return vec4<f32>(value(i), value(i + 1), value(i + 2), value(i + 3));
}

fn g_premul(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.rgb * c.a, c.a);
}

// Premultiplied `top` over premultiplied `bottom`.
fn g_over(top: vec4<f32>, bottom: vec4<f32>) -> vec4<f32> {
    return top + bottom * (1.0 - top.a);
}
