#version 100

precision mediump float;

varying vec2 v_local;
varying vec4 v_rect;
varying vec4 v_radii;
varying vec4 v_color;
varying vec4 v_parameters;

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

float coverage(float distance, float effective_scale) {
    float half_width = 0.5 / max(effective_scale, 0.000001);
    return 1.0 - smoothstep(-half_width, half_width, distance);
}

void main() {
    float kind = v_parameters.x;
    float border_width = v_parameters.y;
    float blur_radius = v_parameters.z;
    float effective_scale = v_parameters.w;
    float distance;
    float alpha;

    if (kind > 2.5) {
        vec2 lower = v_rect.xy - v_local;
        vec2 upper = v_local - (v_rect.xy + v_rect.zw);
        distance = max(max(lower.x, lower.y), max(upper.x, upper.y));
        alpha = coverage(distance, effective_scale);
    } else if (kind < 0.5) {
        distance = rounded_distance(v_local, v_rect, v_radii);
        alpha = coverage(distance, effective_scale);
    } else if (kind < 1.5) {
        distance = rounded_distance(v_local, v_rect, v_radii);
        float outer = coverage(distance, effective_scale);
        vec4 inner_rect = vec4(
            v_rect.xy + vec2(border_width),
            max(v_rect.zw - vec2(border_width * 2.0), 0.0)
        );
        vec4 inner_radii = max(v_radii - vec4(border_width), 0.0);
        float inner = 0.0;
        if (inner_rect.z > 0.0 && inner_rect.w > 0.0) {
            inner = coverage(
                rounded_distance(v_local, inner_rect, inner_radii),
                effective_scale
            );
        }
        alpha = clamp(outer - inner, 0.0, 1.0);
    } else if (blur_radius <= 0.0) {
        distance = rounded_distance(v_local, v_rect, v_radii);
        alpha = coverage(distance, effective_scale);
    } else {
        distance = rounded_distance(v_local, v_rect, v_radii);
        if (distance <= 0.0) {
            alpha = 1.0;
        } else {
            float normalized = distance / blur_radius;
            alpha = exp(-0.5 * normalized * normalized);
        }
    }

    float output_alpha = v_color.a * alpha;
    gl_FragColor = vec4(v_color.rgb * output_alpha, output_alpha);
}
