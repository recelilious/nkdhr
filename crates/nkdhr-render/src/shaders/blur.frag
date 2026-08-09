#version 100

precision mediump float;

uniform sampler2D u_source;
uniform vec2 u_target_size;
uniform float u_radius;

void main() {
    vec2 uv = gl_FragCoord.xy / u_target_size;
    vec2 step_uv = vec2(u_radius / (4.0 * u_target_size.x), 0.0);
    vec4 color = texture2D(u_source, uv) * 0.22702703;
    color += texture2D(u_source, uv - step_uv) * 0.19459459;
    color += texture2D(u_source, uv + step_uv) * 0.19459459;
    color += texture2D(u_source, uv - step_uv * 2.0) * 0.12162162;
    color += texture2D(u_source, uv + step_uv * 2.0) * 0.12162162;
    color += texture2D(u_source, uv - step_uv * 3.0) * 0.054054055;
    color += texture2D(u_source, uv + step_uv * 3.0) * 0.054054055;
    color += texture2D(u_source, uv - step_uv * 4.0) * 0.016216217;
    color += texture2D(u_source, uv + step_uv * 4.0) * 0.016216217;
    gl_FragColor = color;
}
