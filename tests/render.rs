//! End-to-end render tests with no window and, on CI, no GPU.
//!
//! The unit tests cover parsing and meshing; these cover the half that only
//! shows up once pixels exist -- that the camera frames the model, that the
//! palette reaches the shader, that the scene graph moves geometry apart, and
//! that two runs of the same input agree.
//!
//! Nothing here compares against a stored golden image. Rasterisers disagree
//! about edge pixels, and a test that fails when the driver is upgraded is a
//! test people learn to ignore. The assertions are the properties a correct
//! render has on any driver: how much of the frame the model covers, which
//! palette colours appear in it, and where the ink sits.

use voxview::{fixtures, gfx::Renderer, loader, mesh};

const SIZE: u32 = 128;

/// A render, as straight RGBA8.
struct Shot {
    pixels: Vec<u8>,
}

impl Shot {
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * SIZE + x) * 4) as usize;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }

    /// Fraction of the frame the model covers, 0.0 to 1.0.
    fn coverage(&self) -> f32 {
        let lit = self
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 8)
            .count();
        lit as f32 / (SIZE * SIZE) as f32
    }

    /// How far apart the two most different opaque colours are in
    /// chromaticity -- colour normalised by brightness.
    ///
    /// Brightness is the wrong axis to measure on, because Lambert shading
    /// turns one palette entry into a whole ramp of brightnesses. Dividing it
    /// out leaves the part that only differs when the palette actually holds
    /// two colours, which is the thing worth asserting.
    fn chromatic_spread(&self) -> f32 {
        let hues: Vec<[f32; 3]> = self
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 200)
            .filter_map(|p| {
                let sum = p[0] as f32 + p[1] as f32 + p[2] as f32;
                (sum > 24.0).then(|| [p[0] as f32 / sum, p[1] as f32 / sum, p[2] as f32 / sum])
            })
            .collect();
        let mut worst = 0.0f32;
        // The extremes of each axis bound the spread, so this stays linear
        // rather than comparing every pixel with every other one.
        for axis in 0..3 {
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for h in &hues {
                lo = lo.min(h[axis]);
                hi = hi.max(h[axis]);
            }
            if hi > lo {
                worst = worst.max(hi - lo);
            }
        }
        worst
    }

    /// Column occupancy, for asking whether geometry landed in two clumps.
    fn columns(&self) -> Vec<bool> {
        (0..SIZE)
            .map(|x| (0..SIZE).any(|y| self.at(x, y)[3] > 8))
            .collect()
    }

    /// Write the render out so a failing CI run has something to look at.
    fn save(&self, name: &str) {
        let dir = std::path::Path::new("target/render-output");
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        if let Some(image) = image::RgbaImage::from_raw(SIZE, SIZE, self.pixels.clone()) {
            let _ = image.save(dir.join(format!("{name}.png")));
        }
    }
}

/// A renderer, or `None` when this machine has no usable adapter.
///
/// Developers without a working Vulkan or DX12 setup get a skip and a note.
/// CI sets `VOXVIEW_REQUIRE_GPU`, which turns the skip into a failure, so the
/// coverage cannot quietly evaporate on the one machine that matters.
fn renderer() -> Option<Renderer> {
    // One sample everywhere except the test that is about multisampling:
    // these assertions are about geometry and palette, and an antialiased
    // edge only blurs the thing being measured.
    renderer_with(1)
}

fn renderer_with(samples: u32) -> Option<Renderer> {
    match Renderer::headless(SIZE, SIZE, samples) {
        Ok(renderer) => {
            eprintln!("rendering on: {}", renderer.adapter_name);
            Some(renderer)
        }
        Err(e) => {
            if std::env::var_os("VOXVIEW_REQUIRE_GPU").is_some() {
                panic!("VOXVIEW_REQUIRE_GPU is set and no adapter came up: {e:#}");
            }
            eprintln!("skipping: no graphics adapter ({e:#})");
            None
        }
    }
}

/// Render one fixture, by the same path the viewer takes.
fn shoot(renderer: &mut Renderer, data: &dot_vox::DotVoxData, name: &str) -> Shot {
    let dir = std::env::temp_dir().join(format!("voxview-render-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.vox"));
    std::fs::write(&path, fixtures::to_bytes(data)).expect("write fixture");

    let scene = loader::load_file(&path).expect("the fixture parses");
    let meshes = mesh::mesh_models(&scene.models);
    let pixels = renderer
        .thumbnail(&scene, &meshes, SIZE)
        .expect("the render succeeds");
    std::fs::remove_file(&path).ok();

    assert_eq!(
        pixels.len(),
        (SIZE * SIZE * 4) as usize,
        "the render came back the wrong size"
    );
    let shot = Shot { pixels };
    shot.save(name);
    shot
}

#[test]
fn a_cube_renders_framed_and_in_its_own_colour() {
    let Some(mut renderer) = renderer() else {
        return;
    };
    let data = fixtures::single_cube();
    let colour = data.palette[0];
    let shot = shoot(&mut renderer, &data, "cube");

    // Framing puts the model in the middle of the frame at a sane size. Too
    // little and the camera has backed off; too much and it is clipped.
    let coverage = shot.coverage();
    assert!(
        (0.05..0.75).contains(&coverage),
        "a framed cube should fill a useful part of the frame, got {coverage:.3}"
    );

    // The centre of a framed solid cube is always the cube.
    assert!(
        shot.at(SIZE / 2, SIZE / 2)[3] > 200,
        "the middle of the frame should be solid cube"
    );

    // The corners are always background.
    for (x, y) in [(0, 0), (SIZE - 1, 0), (0, SIZE - 1), (SIZE - 1, SIZE - 1)] {
        assert_eq!(
            shot.at(x, y)[3],
            0,
            "the corner at {x},{y} should be transparent background"
        );
    }

    // The palette actually reached the shader. Lambert shading darkens every
    // face by a different amount, so the brightest face is the reference and
    // the tolerance is wide; what is being tested is hue, not exposure.
    let lit = shot
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 200)
        .max_by_key(|p| p[0] as u32 + p[1] as u32 + p[2] as u32)
        .expect("something is lit");
    let (r, g, b) = (lit[0] as f32, lit[1] as f32, lit[2] as f32);
    let (wr, wg, wb) = (colour.r as f32, colour.g as f32, colour.b as f32);
    let dominant = r > g * 1.3 && r > b * 1.3;
    assert_eq!(
        dominant,
        wr > wg * 1.3 && wr > wb * 1.3,
        "rendered {r},{g},{b} does not share a dominant channel with palette {wr},{wg},{wb}"
    );
}

#[test]
fn the_scene_graph_puts_two_models_in_two_places() {
    let Some(mut renderer) = renderer() else {
        return;
    };
    let shot = shoot(
        &mut renderer,
        &fixtures::two_models_translated(),
        "two_models",
    );

    // Two models translated apart leave a gap the camera cannot close. If the
    // transforms were dropped, they would sit on top of each other and the
    // columns would form one run instead of two.
    let columns = shot.columns();
    let runs = columns
        .windows(2)
        .filter(|w| w[0] != w[1])
        .count()
        .div_ceil(2);
    assert!(
        runs >= 2,
        "expected two separated clumps of geometry, found {runs}"
    );
}

#[test]
fn a_checkerboard_keeps_both_of_its_colours() {
    let Some(mut renderer) = renderer() else {
        return;
    };
    let shot = shoot(&mut renderer, &fixtures::checkerboard_8(), "checker");

    // A checkerboard is where a palette bug shows: read the index wrong and
    // every cell comes back the same colour, so the spread collapses.
    let spread = shot.chromatic_spread();
    eprintln!("checkerboard chromatic spread: {spread:.3}");
    assert!(
        spread > 0.08,
        "the checkerboard's two palette colours are not distinguishable in the \
         render (spread {spread:.3}); the palette may not be reaching the shader"
    );
    assert!(
        shot.coverage() > 0.05,
        "the checkerboard rendered almost nothing"
    );
}

#[test]
fn a_single_colour_model_shows_a_single_colour() {
    let Some(mut renderer) = renderer() else {
        return;
    };
    let shot = shoot(&mut renderer, &fixtures::single_cube(), "cube_spread");
    let spread = shot.chromatic_spread();
    eprintln!("solid cube chromatic spread: {spread:.3}");
    assert!(
        spread < 0.08,
        "a cube of one palette colour rendered several hues (spread {spread:.3})"
    );
}

#[test]
fn the_same_input_renders_the_same_bytes_twice() {
    let Some(mut renderer) = renderer() else {
        return;
    };
    let data = fixtures::single_cube();
    let first = shoot(&mut renderer, &data, "determinism_a");
    let second = shoot(&mut renderer, &data, "determinism_b");
    assert_eq!(
        first.pixels, second.pixels,
        "two renders of one fixture disagree; something is carrying state between frames"
    );
}

#[test]
fn multisampling_softens_the_edges_of_the_model() {
    let Some(mut plain) = renderer_with(1) else {
        return;
    };
    let Some(mut smooth) = renderer_with(4) else {
        return;
    };
    if smooth.samples() == 1 {
        eprintln!("skipping: this adapter does not do 4x multisampling");
        return;
    }

    let data = fixtures::single_cube();
    let hard = shoot(&mut plain, &data, "aliased");
    let soft = shoot(&mut smooth, &data, "antialiased");

    // The thumbnail renders on a transparent ground, so a pixel the edge only
    // partly covers comes back partly transparent -- and at one sample there
    // is no such thing: every pixel is all model or all background. Counting
    // the in-between alphas is therefore a direct measure of whether the
    // resolve happened at all, on any rasteriser.
    let partial = |shot: &Shot| {
        shot.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| (8..248).contains(&p[3]))
            .count()
    };
    let (hard_edge, soft_edge) = (partial(&hard), partial(&soft));
    eprintln!(
        "edge pixels: {hard_edge} at 1x, {soft_edge} at {}x",
        smooth.samples()
    );
    assert_eq!(
        hard_edge, 0,
        "one sample per pixel should leave no partly covered pixels"
    );
    assert!(
        soft_edge > 32,
        "multisampling should leave a rim of partly covered pixels, found {soft_edge}"
    );

    // It is the same cube either way: antialiasing is an edge treatment, not
    // a change of framing.
    let (a, b) = (hard.coverage(), soft.coverage());
    assert!(
        (a - b).abs() < 0.02,
        "coverage moved from {a:.3} to {b:.3}; that is more than an edge"
    );
}

#[test]
fn a_model_that_will_not_parse_never_reaches_the_renderer() {
    // The guarantee the brief asks for, checked at the seam where a bad file
    // would otherwise become a bad draw call.
    let dir = std::env::temp_dir().join(format!("voxview-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("truncated.vox");
    let mut bytes = fixtures::to_bytes(&fixtures::single_cube());
    bytes.truncate(bytes.len() / 2);
    std::fs::write(&path, &bytes).expect("write");

    let result = loader::load_file(&path);
    std::fs::remove_file(&path).ok();
    assert!(result.is_err(), "a truncated file should not load");
}
