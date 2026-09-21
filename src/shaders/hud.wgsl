// HUD overlay: screen-space quads sampling a single-channel bitmap font
// atlas. Positions arrive in physical pixels with the origin at the top left.

struct Globals {
    view_proj: mat4x4<f32>,
    light_dir: vec4<f32>,
    params: vec4<f32>,
    viewport: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var font_texture: texture_2d<f32>;
@group(1) @binding(1) var font_sampler: sampler;

struct VsIn {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    let ndc = vec2<f32>(
        in.position.x / max(globals.viewport.x, 1.0) * 2.0 - 1.0,
        1.0 - in.position.y / max(globals.viewport.y, 1.0) * 2.0,
    );
    var out: VsOut;
    out.clip_position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let coverage = textureSample(font_texture, font_sampler, in.uv).r;
    let alpha = in.color.a * coverage;
    // Pre-multiplied would need a different blend state; keep it straight
    // alpha to match the pipeline's blend configuration.
    return vec4<f32>(in.color.rgb, alpha);
}
