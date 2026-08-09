#version 100

precision mediump float;

uniform sampler2D u_original;
uniform sampler2D u_horizontal;
uniform vec2 u_target_size;
uniform float u_radius;
uniform float u_effective_scale;
uniform vec4 u_rect;
uniform vec4 u_radii;

varying vec2 v_local;

float corner_radius(vec2 point, vec4 rect, vec4 radii) {
    vec2 center = rect.xy + rect.zw * 0.5;
    if (point.x < center.x) {
        return point.y < center.y ? radii.x : radii.w;
    }
    return point.y < center.y ? radii.y : radii.z;
}

float rounded_distance(vec2 point, vec4 rect, vec4 radii) {
    vec2 center = rect.xy + rect.zw * 0.5;
    float radius = corner_radius(point, rect, radii);
    vec2 q = abs(point - center) - rect.zw * 0.5 + vec2(radius);
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - radius;
}

void main() {
    vec2 uv = gl_FragCoord.xy / u_target_size;
    vec2 step_uv = vec2(0.0, u_radius / (4.0 * u_target_size.y));
    vec4 blurred = texture2D(u_horizontal, uv) * 0.22702703;
    blurred += texture2D(u_horizontal, uv - step_uv) * 0.19459459;
    blurred += texture2D(u_horizontal, uv + step_uv) * 0.19459459;
    blurred += texture2D(u_horizontal, uv - step_uv * 2.0) * 0.12162162;
    blurred += texture2D(u_horizontal, uv + step_uv * 2.0) * 0.12162162;
    blurred += texture2D(u_horizontal, uv - step_uv * 3.0) * 0.054054055;
    blurred += texture2D(u_horizontal, uv + step_uv * 3.0) * 0.054054055;
    blurred += texture2D(u_horizontal, uv - step_uv * 4.0) * 0.016216217;
    blurred += texture2D(u_horizontal, uv + step_uv * 4.0) * 0.016216217;

    float half_width = 0.5 / max(u_effective_scale, 0.000001);
    float coverage = 1.0 - smoothstep(
        -half_width,
        half_width,
        rounded_distance(v_local, u_rect, u_radii)
    );
    vec4 original = texture2D(u_original, uv);
    gl_FragColor = mix(original, blurred, coverage);
}
