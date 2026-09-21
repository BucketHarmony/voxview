// Voxel faces. One draw call per model instance; the palette lives in a
// uniform so vertices only carry an index into it.

struct Globals {
    view_proj: mat4x4<f32>,
    // xyz: direction the light travels. w unused.
    light_dir: vec4<f32>,
    // x: ambient term, y: AO strength (0 or 1), zw unused.
    params: vec4<f32>,
    // xy: framebuffer size in pixels.
    viewport: vec4<f32>,
};

struct Palette {
    colors: array<vec4<f32>, 256>,
};

struct ModelUniform {
    model: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var<uniform> palette: Palette;
@group(1) @binding(0) var<uniform> model_uniform: ModelUniform;

struct VsIn {
    @location(0) position: vec3<f32>,
    // Bits 0-7 palette index, 8-10 face index, 11-18 ambient occlusion.
    @location(1) packed: u32,
};

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    let model = model_uniform.model;
    let world = model * vec4<f32>(in.position, 1.0);

    let palette_index = in.packed & 0xffu;
    let face = (in.packed >> 8u) & 7u;
    let ao = f32((in.packed >> 11u) & 0xffu) / 255.0;

    // Face index 0..5 is +X, -X, +Y, -Y, +Z, -Z. Built without indexing so
    // there is no dynamic index into a constant array.
    let axis = face >> 1u;
    let sign = select(-1.0, 1.0, (face & 1u) == 0u);
    let normal = vec3<f32>(
        select(0.0, sign, axis == 0u),
        select(0.0, sign, axis == 1u),
        select(0.0, sign, axis == 2u),
    );

    // The model matrix is a signed permutation plus a translation, so its
    // upper-left 3x3 block already is the normal matrix.
    let rotation = mat3x3<f32>(model[0].xyz, model[1].xyz, model[2].xyz);
    let world_normal = normalize(rotation * normal);

    let to_light = -normalize(globals.light_dir.xyz);
    let lambert = max(dot(world_normal, to_light), 0.0);
    let ambient = globals.params.x;
    let occlusion = mix(1.0, ao, globals.params.y);
    let intensity = (ambient + (1.0 - ambient) * lambert) * occlusion;

    var out: VsOut;
    out.clip_position = globals.view_proj * world;
    out.color = palette.colors[palette_index].rgb * intensity;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Opaque: the alpha channel is what makes a screenshot's background
    // transparent, so geometry must always write 1.0 there.
    return vec4<f32>(in.color, 1.0);
}
