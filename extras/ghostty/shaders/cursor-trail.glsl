// Cursor trail for Ghostty (1.2+ cursor uniforms), done the way kitty's cursor_trail works
// (kitty.conf: cursor_trail 1, cursor_trail_decay 0.1 0.5): each corner of the cursor slides from
// where it was to where it is, the corners facing the move arriving in FAST seconds and the ones
// behind in SLOW, and the quad between them is filled solid in the cursor's colour.
// Ghostty gives cursor rects as (left, top, width, height) in pixels, origin bottom-left like
// fragCoord (measured 2026-11-01; so the bottom edge is y - height).
//
// Pacing: Ghostty 1.4 redraws an animating shader from a fixed 8 ms timer on its render thread
// (src/renderer/generic.zig draw_interval_ms), not from the compositor's frame callbacks, and on
// Linux it has no vsync (hasVsync is macOS-only). Nothing a shader does can change that cadence;
// everything below is a pure function of iTime so a skipped frame is one missing step of the
// slide, never a stall that catches up later.
const float FAST = 0.1;
const float SLOW = 0.5;

float cross2(vec2 a, vec2 b) { return a.x * b.y - a.y * b.x; }

float edgeDist(vec2 p, vec2 a, vec2 b) {
    vec2 pa = p - a, ba = b - a;
    float h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-4), 0.0, 1.0);
    return length(pa - ba * h);
}

bool inTri(vec2 p, vec2 a, vec2 b, vec2 c) {
    float d1 = cross2(b - a, p - a), d2 = cross2(c - b, p - b), d3 = cross2(a - c, p - c);
    return !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0));
}

vec2 corner(vec4 r, int i) { // 0 top-left, 1 top-right, 2 bottom-right, 3 bottom-left
    return r.xy + vec2(i == 1 || i == 2 ? r.z : 0.0, i >= 2 ? -r.w : 0.0);
}

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec4 base = texture(iChannel0, fragCoord / iResolution.xy);
    fragColor = base;
    vec4 cur = iCurrentCursor, prev = iPreviousCursor;
    // unfocused, Ghostty stops animating and the last frame would stay up
    if (iFocus == 0) return;
    // a program hid the cursor (most TUIs do while they draw): no trail to a cursor nobody sees
    if (iCursorVisible == 0) return;

    vec2 c1 = cur.xy + vec2(cur.z, -cur.w) * 0.5;
    vec2 c0 = prev.xy + vec2(prev.z, -prev.w) * 0.5;
    vec2 move = c1 - c0;
    if (dot(move, move) < 1.0) return;
    vec2 dir = normalize(move);
    float t = iTime - iTimeCursorChange;
    if (t > SLOW) return; // all corners home: no 1 px fringe left around the cursor

    vec2 q[4];
    float left = 0.0; // farthest any corner still has to go
    vec2 lo = vec2(1e9), hi = vec2(-1e9);
    for (int i = 0; i < 4; i++) {
        vec2 to = corner(cur, i);
        // 1 = facing the move. Corners count as pure diagonals: with the real offsets of a tall,
        // narrow cursor every corner points mostly up or down and the whole trail drifted off
        // the cursor instead of stretching from it
        float lead = dot(normalize(sign(to - c1)), dir) * 0.5 + 0.5;
        float decay = mix(SLOW, FAST, lead);
        float k = 1.0 - exp2(-10.0 * t / decay);                 // ~done after `decay` seconds
        q[i] = mix(corner(prev, i), to, k);
        left = max(left, distance(q[i], to));
        lo = min(lo, q[i]);
        hi = max(hi, q[i]);
    }
    // like kitty, drop the trail once it is down to a sliver instead of shrinking it for 0.5 s
    if (left < 0.6 * cur.z) return;
    // most of the screen is nowhere near the trail: skip the triangle and edge maths there
    // (1 px margin for the antialiased edge)
    if (any(lessThan(fragCoord, lo - 1.0)) || any(greaterThan(fragCoord, hi + 1.0))) return;

    // keep the cursor itself (and the character under it) as Ghostty drew it
    if (fragCoord.x >= cur.x && fragCoord.x <= cur.x + cur.z && fragCoord.y <= cur.y && fragCoord.y >= cur.y - cur.w) return;
    // the quad can fold while the corners travel: fill the union of both diagonal splits
    bool inside = inTri(fragCoord, q[0], q[1], q[2]) || inTri(fragCoord, q[0], q[2], q[3])
               || inTri(fragCoord, q[1], q[2], q[3]) || inTri(fragCoord, q[1], q[3], q[0]);
    float d = min(min(edgeDist(fragCoord, q[0], q[1]), edgeDist(fragCoord, q[1], q[2])),
                  min(edgeDist(fragCoord, q[2], q[3]), edgeDist(fragCoord, q[3], q[0])));
    float a = inside ? 1.0 : clamp(1.0 - d, 0.0, 1.0);          // 1 px antialiased edge
    fragColor = vec4(mix(base.rgb, iCurrentCursorColor.rgb, a), max(base.a, a));
}
