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

use std::sync::{Mutex, OnceLock};
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

    /// Mean alpha of the pixels a vertical band actually covered, ignoring
    /// the ones nothing drew on. `None` when the band is empty.
    fn band_alpha(&self, from: u32, to: u32) -> Option<f32> {
        let mut total = 0.0f32;
        let mut count = 0usize;
        for x in from..to.min(SIZE) {
            for y in 0..SIZE {
                let a = self.at(x, y)[3];
                if a > 0 {
                    total += a as f32;
                    count += 1;
                }
            }
        }
        (count > 0).then(|| total / count as f32)
    }

    /// Mean brightness of the opaque pixels in a vertical band of the frame,
    /// or `None` when the band is empty.
    fn band_brightness(&self, from: u32, to: u32) -> Option<f32> {
        let mut total = 0.0f32;
        let mut count = 0usize;
        for x in from..to.min(SIZE) {
            for y in 0..SIZE {
                let p = self.at(x, y);
                if p[3] > 200 {
                    total += (p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.0;
                    count += 1;
                }
            }
        }
        (count > 0).then(|| total / count as f32)
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

/// One graphics device for the whole file, handed to one test at a time.
///
/// Every test used to build its own. That is fine on a desktop GPU and a coin
/// flip on a CI box with a software adapter: nine devices being created and
/// torn down at once took the Windows runner's driver down with an access
/// violation, in a test binary whose whole subject is that bad input does not
/// crash anything. One device and one lock removes the question, and the
/// suite runs faster for it.
static SHARED: OnceLock<Mutex<Option<Renderer>>> = OnceLock::new();

/// The shared renderer at one sample per pixel.
///
/// `None` when this machine has no usable adapter: developers without a
/// working Vulkan or DX12 setup get a skip and a note. CI sets
/// `VOXVIEW_REQUIRE_GPU`, which turns the skip into a failure, so the coverage
/// cannot quietly evaporate on the one machine that matters.
///
/// One sample except where a test says otherwise, because these assertions are
/// about geometry and palette and an antialiased edge only blurs what is being
/// measured.
fn renderer() -> Option<Lease> {
    let mut guard = SHARED
        .get_or_init(|| Mutex::new(open_renderer()))
        // A panicking test leaves the renderer perfectly usable; refusing to
        // hand it out after one failure would turn one red test into eight.
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.as_mut()?.set_samples(1);
    Some(Lease { guard })
}

fn open_renderer() -> Option<Renderer> {
    match Renderer::headless(SIZE, SIZE, 1) {
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

/// The shared renderer, borrowed for as long as one test runs.
struct Lease {
    guard: std::sync::MutexGuard<'static, Option<Renderer>>,
}

impl std::ops::Deref for Lease {
    type Target = Renderer;

    fn deref(&self) -> &Renderer {
        self.guard.as_ref().expect("a lease implies a renderer")
    }
}

impl std::ops::DerefMut for Lease {
    fn deref_mut(&mut self) -> &mut Renderer {
        self.guard.as_mut().expect("a lease implies a renderer")
    }
}

/// Render one fixture, by the same path the viewer takes.
fn shoot(renderer: &mut Renderer, data: &dot_vox::DotVoxData, name: &str) -> Shot {
    let dir = std::env::temp_dir().join(format!("voxview-render-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    // Tests run in parallel and two of them may want the same fixture, so the
    // file name has to be unique per call rather than per fixture: otherwise
    // one test deletes the file the other is still reading.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = dir.join(format!("{name}-{serial}.vox"));
    std::fs::write(&path, fixtures::to_bytes(data)).expect("write fixture");

    let scene = loader::load_file(&path).expect("the fixture parses");
    let meshes = mesh::mesh_models(&scene.models, &scene.materials);
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
    let Some(mut renderer) = renderer() else {
        return;
    };
    let data = fixtures::single_cube();
    let hard = shoot(&mut renderer, &data, "aliased");

    let samples = renderer.set_samples(4);
    if samples == 1 {
        eprintln!("skipping: this adapter does not do 4x multisampling");
        return;
    }
    let soft = shoot(&mut renderer, &data, "antialiased");

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
    eprintln!("edge pixels: {hard_edge} at 1x, {soft_edge} at {samples}x");
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
fn an_emissive_material_lights_its_own_voxels() {
    let Some(mut renderer) = renderer() else {
        return;
    };
    let shot = shoot(&mut renderer, &fixtures::material_blocks(), "materials");

    // Four blocks of one colour in a row along +X, and the default thumbnail
    // camera looks at them from front-right, so they stay left-to-right on
    // screen. Only the second is emissive.
    let bands = material_bands(&shot);
    let diffuse = shot
        .band_brightness(bands[0].0, bands[0].1)
        .expect("the diffuse block drew");
    let emissive = shot
        .band_brightness(bands[1].0, bands[1].1)
        .expect("the emissive block drew");
    eprintln!("brightness: diffuse {diffuse:.1}, emissive {emissive:.1}");

    // Same palette colour, same light, same angles: the only thing that can
    // separate them is the MATL chunk.
    assert!(
        emissive > diffuse * 1.2,
        "an emissive block should be clearly brighter than an identically \
         coloured diffuse one ({emissive:.1} vs {diffuse:.1})"
    );
}

#[test]
fn a_glass_material_lets_the_background_through() {
    let Some(mut renderer) = renderer() else {
        return;
    };
    let shot = shoot(&mut renderer, &fixtures::material_blocks(), "materials");

    let bands = material_bands(&shot);
    let diffuse = shot
        .band_alpha(bands[0].0, bands[0].1)
        .expect("the diffuse block drew");
    let glass = shot
        .band_alpha(bands[3].0, bands[3].1)
        .expect("the glass block drew");
    eprintln!("alpha: diffuse {diffuse:.1}, glass {glass:.1}");

    // The thumbnail clears to transparent, so a blended face leaves its own
    // opacity behind and an opaque one leaves 255. Anything less than opaque
    // means the face went through the blended pass, which is the whole claim.
    assert!(
        diffuse > 250.0,
        "a diffuse block should be fully opaque ({diffuse:.1})"
    );
    assert!(
        glass < 200.0,
        "a glass block should not be ({glass:.1} against {diffuse:.1})"
    );
}

/// The four material blocks' horizontal extents, as pixel columns.
///
/// They are evenly spaced along +X and the camera keeps them left to right,
/// so quartering what actually drew finds each one without having to project
/// anything by hand.
fn material_bands(shot: &Shot) -> [(u32, u32); 4] {
    let columns = shot.columns();
    let first = columns.iter().position(|&on| on).expect("something drew") as u32;
    let last = columns.iter().rposition(|&on| on).expect("something drew") as u32;
    let quarter = (last - first + 1) / 4;
    std::array::from_fn(|i| (first + i as u32 * quarter, first + (i as u32 + 1) * quarter))
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
