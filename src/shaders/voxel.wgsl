// Voxel faces. One draw call per model instance; the palette and the material
// table live in uniforms so vertices only carry an index into both.

struct Globals {
    view_proj: mat4x4<f32>,
    // xyz: direction the light travels. w unused.
    light_dir: vec4<f32>,
    // x: ambient term, y: AO strength (0 or 1), zw unused.
    params: vec4<f32>,
    // xy: framebuffer size in pixels.
    viewport: vec4<f32>,
    // xyz: camera position in world space, for the specular view vector.
    eye: vec4<f32>,
};

struct Palette {
    colors: array<vec4<f32>, 256>,
};

// One entry per palette index: x emission, y metalness, z roughness,
// w opacity.
struct Materials {
    entries: array<vec4<f32>, 256>,
};

struct ModelUniform {
    model: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var<uniform> palette: Palette;
@group(0) @binding(2) var<uniform> materials: Materials;
@group(1) @binding(0) var<uniform> model_uniform: ModelUniform;

struct VsIn {
    @location(0) position: vec3<f32>,
    // Bits 0-7 palette index, 8-10 face index, 11-18 ambient occlusion.
    @location(1) packed: u32,
};

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    // Flat: a quad's palette index is the same at all four corners, and
    // interpolating an index would be meaningless.
    @location(0) @interpolate(flat) palette_index: u32,
    @location(1) world_normal: vec3<f32>,
    @location(2) world_position: vec3<f32>,
    @location(3) occlusion: f32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    let model = model_uniform.model;
    let world = model * vec4<f32>(in.position, 1.0);

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

    var out: VsOut;
    out.clip_position = globals.view_proj * world;
    out.palette_index = in.packed & 0xffu;
    out.world_normal = normalize(rotation * normal);
    out.world_position = world.xyz;
    out.occlusion = mix(1.0, ao, globals.params.y);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let entry = palette.colors[in.palette_index];
    let base = entry.rgb;
    let material = materials.entries[in.palette_index];
    let emit = material.x;
    let metal = material.y;
    let rough = material.z;

    let normal = normalize(in.world_normal);
    let to_light = -normalize(globals.light_dir.xyz);
    let lambert = max(dot(normal, to_light), 0.0);
    let ambient = globals.params.x;
    let diffuse = (ambient + (1.0 - ambient) * lambert) * in.occlusion;

    // Blinn-Phong, and only for metals. Greedy meshing merges a flat wall
    // into one enormous quad, so a highlight computed per vertex would smear
    // across it; per fragment it stays where the geometry says it is. Nothing
    // else gets a highlight, so a file with no materials renders exactly as
    // it did before this shader knew what a material was.
    var specular = vec3<f32>(0.0);
    if (metal > 0.0) {
        let to_eye = normalize(globals.eye.xyz - in.world_position);
        let half_vector = normalize(to_light + to_eye);
        // Roughness to a Phong exponent, floored so a mirror-smooth surface
        // does not ask for an exponent the hardware cannot hold.
        let r = clamp(rough, 0.05, 1.0);
        let shininess = clamp(2.0 / (r * r * r * r) - 2.0, 2.0, 2048.0);
        let peak = pow(max(dot(normal, half_vector), 0.0), shininess);
        // Metals tint their highlight with their own colour; the step towards
        // white keeps a dark metal from looking matte.
        let tint = mix(vec3<f32>(1.0), base, 0.65);
        specular = tint * peak * metal * step(0.0, lambert) * in.occlusion;
    }

    var color = base * diffuse + specular;

    // An emissive surface makes its own light, so shading and occlusion stop
    // applying to it: the blend weight is how emissive it is, and the scale
    // above 1 is what a display without any high dynamic range has instead of
    // a bloom -- it clips towards white, which is what looking at a lamp does.
    if (emit > 0.0) {
        color = mix(color, base * (1.0 + emit), clamp(emit, 0.0, 1.0));
    }

    // Opaque geometry must write 1.0: the alpha channel is what makes a
    // screenshot's background transparent.
    return vec4<f32>(color, 1.0);
}
