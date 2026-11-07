#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// gl_FragCoord -> window-geometry-local physical pixels
uniform mat3 frag_to_geo;
uniform vec2 geo_size;
uniform float radius;
uniform float dim;

float rounded_box(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - (half_size - vec2(r));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif
    vec2 p = (frag_to_geo * vec3(gl_FragCoord.xy, 1.0)).xy;
    float d = rounded_box(p - geo_size * 0.5, geo_size * 0.5, radius);
    float cov = clamp(0.5 - d, 0.0, 1.0);
    color.rgb *= (1.0 - dim);
    color = color * alpha * cov;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
