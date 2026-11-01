// Neon comet cursor trail for Ghostty: when the cursor moves, a solid comet in the cursor's colour
// runs from where it was to where it is, its head (as wide as the cursor) arriving in FAST
// seconds and its pointed tail following in SLOW, with a blue glow around it. (kitty's trail
// slides the four corners at different speeds, which made a diamond; the owner chose a comet.)
// Ghostty gives cursor rects as (left, top, width, height) in pixels, origin bottom-left like
// fragCoord.
const float FAST = 0.1;
const float SLOW = 0.5;
const vec3 NEON = vec3(0.16, 0.2, 1.0); // halo; shortcut: fixed colour, not from `theme`

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec4 base = texture(iChannel0, fragCoord / iResolution.xy);
    fragColor = base;
    vec4 cur = iCurrentCursor, prev = iPreviousCursor;
    // unfocused, Ghostty stops animating and the last frame would stay up
    if (iFocus == 0) return;

    vec2 c1 = cur.xy + vec2(cur.z, -cur.w) * 0.5;
    vec2 c0 = prev.xy + vec2(prev.z, -prev.w) * 0.5;
    vec2 move = c1 - c0;
    if (dot(move, move) < 1.0) return;
    float t = iTime - iTimeCursorChange;
    if (t > SLOW) return;

    vec2 dir = normalize(move), n = vec2(-dir.y, dir.x);
    // the tail starts at the old cursor's back edge and the head ends at the new one's front edge,
    // so even a typed character (one cell) leaves a cell-long tail behind the cursor; from the
    // centres, the cursor itself covered almost all of it
    float e = 0.5 * (abs(dir.x) * cur.z + abs(dir.y) * cur.w);
    vec2 head = mix(c0, c1, 1.0 - exp2(-10.0 * t / FAST)) + dir * e;
    vec2 tail = mix(c0, c1, 1.0 - exp2(-10.0 * t / SLOW)) - dir * e;
    // like kitty, drop the trail once the tail is nearly home instead of shrinking it for 0.5 s
    if (distance(tail, c1 - dir * e) < 0.6 * cur.z) return;
    float len = distance(head, tail);
    if (len < 0.5) return;
    // the head is as wide as the cursor seen across the move
    float r = 0.5 * (abs(n.x) * cur.z + abs(n.y) * cur.w);

    // keep the cursor itself (and the character under it) as Ghostty drew it
    if (fragCoord.x >= cur.x && fragCoord.x <= cur.x + cur.z && fragCoord.y <= cur.y && fragCoord.y >= cur.y - cur.w) return;

    // a cone from the pointed tail (u = 0) to the flat head (u = 1)
    vec2 p = fragCoord - tail;
    float u = dot(p, dir) / len, uc = clamp(u, 0.0, 1.0);
    float d = length(vec2((u - uc) * len, max(abs(dot(p, n)) - r * uc, 0.0)));
    float a = clamp(1.0 - d, 0.0, 1.0);                       // solid inside, 1 px antialiased edge
    // typing (one cell) gets a faint halo, jumps the full neon: a blue flash on every key was loud
    float strength = mix(0.35, 1.2, smoothstep(cur.z, 4.0 * cur.z, length(move)));
    float glow = exp(-d / 7.0) * strength * (1.0 - a);       // neon halo, added like light
    vec3 rgb = mix(base.rgb, iCurrentCursorColor.rgb, a) + NEON * glow;
    fragColor = vec4(rgb, max(base.a, max(a, clamp(glow, 0.0, 1.0))));
}
