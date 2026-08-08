#version 100

uniform mat3 u_projection;

attribute vec2 a_position;
attribute vec2 a_local;
attribute vec4 a_rect;
attribute vec4 a_radii;
attribute vec4 a_color;
attribute vec4 a_parameters;

varying vec2 v_local;
varying vec4 v_rect;
varying vec4 v_radii;
varying vec4 v_color;
varying vec4 v_parameters;

void main() {
    gl_Position = vec4(u_projection * vec3(a_position, 1.0), 1.0);
    v_local = a_local;
    v_rect = a_rect;
    v_radii = a_radii;
    v_color = a_color;
    v_parameters = a_parameters;
}
