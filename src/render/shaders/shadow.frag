precision highp float;
varying vec2 v_coords;
uniform vec2 size;
uniform float alpha;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec4 shadow_rect;  // x y w h in element-local px
uniform vec4 win_rect;     // window (incl. border) rect, element-local px
uniform float radius;
uniform float win_radius;
uniform float sigma;
uniform vec4 color;

float rounded_box(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - (half_size - vec2(r));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

float erf_approx(float x) {
    float s = sign(x);
    float a = abs(x);
    x = 1.0 + (0.278393 + (0.230389 + 0.078108 * (a * a)) * a) * a;
    x *= x;
    return s - s / (x * x);
}

void main() {
    vec2 p = v_coords * size;
    float d = rounded_box(p - (shadow_rect.xy + shadow_rect.zw * 0.5), shadow_rect.zw * 0.5, radius);
    float a = 0.5 - 0.5 * erf_approx(d / (max(sigma, 0.5) * 1.41421356));
    float dw = rounded_box(p - (win_rect.xy + win_rect.zw * 0.5), win_rect.zw * 0.5, win_radius);
    a *= clamp(dw + 0.5, 0.0, 1.0);
    gl_FragColor = color * a * alpha;
}
