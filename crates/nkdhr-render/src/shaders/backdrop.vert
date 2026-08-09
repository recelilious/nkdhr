#version 100

uniform mat3 u_projection;

attribute vec2 a_position;
attribute vec2 a_local;

varying vec2 v_local;

void main() {
    gl_Position = vec4(u_projection * vec3(a_position, 1.0), 1.0);
    v_local = a_local;
}
