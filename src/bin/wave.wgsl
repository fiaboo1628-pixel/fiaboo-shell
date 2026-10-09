// "squares & smoke bg" by an unknown Shadertoy author, https://www.shadertoy.com/view/MdVXzw
// (CC BY-NC-SA 3.0, Shadertoy's default), ported to WGSL. Added for fiaboo: `bass` brightens the
// smoke and the squares; the CPU speeds `time` up with the loudness and lays out the squares once
// per frame (the per-pixel loop of 60 rotations had an integrated GPU at ~87 % busy).

struct U {
    res: vec2<f32>,
    time: f32,
    bass: f32,
    // per square: [2i] = (x, y, size, _), [2i + 1] = (cos, sin of its turn, centre on screen)
    sq: array<vec4<f32>, 120>,
};
@group(0) @binding(0) var<uniform> u: U;

const BG = vec3<f32>(0.01, 0.16, 0.42);
const RECT = vec3<f32>(0.01, 0.26, 0.57);
const MIN_SIZE = 0.03;
const MAX_SIZE = 0.05; // 0.08 - MIN_SIZE

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // one triangle covering the screen
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u)) * 2.0 - 1.0;
    return vec4<f32>(p, 0.0, 1.0);
}

fn random(co: vec2<f32>) -> f32 {
    return fract(sin(dot(co, vec2<f32>(12.9898, 78.233))) * 43758.5453);
}

fn noise(p0: vec2<f32>) -> f32 {
    let p = p0 * 2.8;
    let i = floor(p);
    let f = fract(p);
    let uu = f * f * (3.0 - 2.0 * f);
    return mix(mix(random(i), random(i + vec2<f32>(1.0, 0.0)), uu.x),
               mix(random(i + vec2<f32>(0.0, 1.0)), random(i + vec2<f32>(1.0, 1.0)), uu.x), uu.y);
}

fn fbm(uv0: vec2<f32>) -> f32 {
    var uv = uv0 * 5.0;
    let m = mat2x2<f32>(1.6, 1.2, -1.2, 1.6);
    var f = 0.5 * noise(uv);
    uv = m * uv;
    f += 0.25 * noise(uv);
    uv = m * uv;
    f += 0.125 * noise(uv);
    uv = m * uv;
    f += 0.0625 * noise(uv);
    return 0.5 + 0.5 * f;
}

fn bg(uv0: vec2<f32>) -> vec3<f32> {
    var uv = uv0;
    let velocity = u.time / 1.6;
    let intensity = sin(uv.x * 3.0 + velocity * 2.0) * 1.1 + 1.5;
    uv.y -= 2.0;
    let bp = uv + vec2<f32>(-2.0, 0.0);
    uv *= 0.6;
    let rb = fbm(vec2<f32>(uv.x * 0.5 - velocity * 0.03, uv.y)) * 0.1;
    uv += rb;
    var rz = fbm(uv * 0.9 + vec2<f32>(velocity * 0.35, 0.0));
    rz *= dot(bp * intensity, bp) + 1.2;
    let col = BG / (0.1 - rz);
    return sqrt(abs(col));
}

fn rectangle(uv: vec2<f32>, pos: vec2<f32>, w: f32, h: f32, blur: f32) -> f32 {
    let d = smoothstep(vec2<f32>(0.0), vec2<f32>(blur), (vec2<f32>(w, h) + 0.01) / 2.0 - abs(uv - pos));
    return d.x * d.y;
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let fc = vec2<f32>(frag.x, u.res.y - frag.y); // Shadertoy's origin is bottom-left
    var uv = fc / u.res * 2.0 - 1.0;
    uv.x *= u.res.x / u.res.y;

    var color = bg(uv) * (2.0 - abs(uv.y * 2.0)) * (1.0 + 0.5 * u.bass);

    for (var i = 0u; i < 60u; i++) {
        let a = u.sq[2u * i];
        let b = u.sq[2u * i + 1u];
        let d = uv - b.zw;
        if (dot(d, d) > 0.0064) { // farther than any square reaches, blur included
            continue;
        }
        let h = a.z / 2.0;
        let v = uv - a.xy + h;
        let uv_rot = vec2<f32>(b.x * v.x + b.y * v.y, -b.y * v.x + b.x * v.y) + a.xy + h; // rotate2d(i + time / 2)
        let rect = rectangle(uv_rot, a.xy, a.z, a.z, (MAX_SIZE + MIN_SIZE - a.z) / 2.0);
        color += RECT * rect * a.z / MAX_SIZE * (1.0 + u.bass);
    }
    return vec4<f32>(color, 1.0);
}
