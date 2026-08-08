#version 100

precision mediump float;

uniform sampler2D u_texture;
uniform float u_alpha_mode;
uniform float u_texture_format;

varying vec2 v_uv;
varying vec4 v_tint;
varying float v_opacity;

void main() {
    vec4 sampled = texture2D(u_texture, v_uv);
    if (u_texture_format > 0.5) {
        float output_alpha = sampled.a * v_tint.a * v_opacity;
        gl_FragColor = vec4(v_tint.rgb * output_alpha, output_alpha);
        return;
    }
    vec4 premultiplied;
    if (u_alpha_mode < 0.5) {
        premultiplied = vec4(sampled.rgb * sampled.a, sampled.a);
    } else if (u_alpha_mode < 1.5) {
        premultiplied = sampled;
    } else {
        premultiplied = vec4(sampled.rgb, 1.0);
    }
    float tint_alpha = v_tint.a;
    gl_FragColor = vec4(
        premultiplied.rgb * v_tint.rgb * tint_alpha,
        premultiplied.a * tint_alpha
    ) * v_opacity;
}
