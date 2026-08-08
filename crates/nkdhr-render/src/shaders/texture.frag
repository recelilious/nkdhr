#version 100

precision mediump float;

uniform sampler2D u_texture;
uniform float u_alpha_mode;

varying vec2 v_uv;
varying float v_opacity;

void main() {
    vec4 sampled = texture2D(u_texture, v_uv);
    vec4 premultiplied;
    if (u_alpha_mode < 0.5) {
        premultiplied = vec4(sampled.rgb * sampled.a, sampled.a);
    } else if (u_alpha_mode < 1.5) {
        premultiplied = sampled;
    } else {
        premultiplied = vec4(sampled.rgb, 1.0);
    }
    gl_FragColor = premultiplied * v_opacity;
}
