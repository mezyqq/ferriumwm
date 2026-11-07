precision highp float;
varying vec2 v_coords;
uniform vec2 size;
uniform float alpha;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform float border;
uniform float radius;
uniform vec4 color1;
uniform vec4 color2;
uniform float angle;

float rounded_box(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - (half_size - vec2(r));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    vec2 p = v_coords * size - size * 0.5;
    float outer = rounded_box(p, size * 0.5, radius);
    float inner_r = max(radius - border, 0.0);
    float inner = rounded_box(p, size * 0.5 - vec2(border), inner_r);
    float a = clamp(0.5 - outer, 0.0, 1.0) * clamp(0.5 + inner, 0.0, 1.0);
    // CSS angle: 0deg points up, 90deg right
    vec2 dir = vec2(sin(angle), -cos(angle));
    float len = abs(size.x * dir.x) + abs(size.y * dir.y);
    float t = clamp(dot(p, dir) / max(len, 1.0) + 0.5, 0.0, 1.0);
    vec4 c = mix(color1, color2, t);
    gl_FragColor = c * a * alpha;
}
