#version 100

uniform mat3 u_projection;

attribute vec2 a_position;
attribute vec2 a_uv;
attribute vec4 a_tint;
attribute float a_opacity;

varying vec2 v_uv;
varying vec4 v_tint;
varying float v_opacity;

void main() {
    gl_Position = vec4(u_projection * vec3(a_position, 1.0), 1.0);
    v_uv = a_uv;
    v_tint = a_tint;
    v_opacity = a_opacity;
}
