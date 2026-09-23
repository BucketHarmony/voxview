//! End-to-end checks over the synthetic fixtures.
//!
//! The `.vox` files under `tests/fixtures/` are written from code by
//! [`voxview::fixtures`], so nothing here depends on MagicaVoxel being
//! installed. They are committed as well, which makes them easy to open in the
//! viewer by hand; this module rewrites them so a stale copy cannot hide a
//! change in how they are built.

use glam::{IVec3, Vec3};
use std::path::PathBuf;
use std::sync::OnceLock;
use voxview::fixtures::{self, TWO_MODELS_T0, TWO_MODELS_T1};
use voxview::{loader, mesh};

/// Write the fixtures once, however many tests run in parallel.
fn fixtures_dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        fixtures::write_all(&dir).expect("could not write the fixtures");
        dir
    })
}

fn load(name: &str) -> loader::VoxScene {
    loader::load_file(&fixtures_dir().join(name)).expect("fixture should parse")
}

#[test]
fn every_fixture_is_written_and_parses() {
    for (name, _) in fixtures::FIXTURES {
        let path = fixtures_dir().join(name);
        assert!(path.is_file(), "{name} was not written");
        let scene = load(name);
        assert_eq!(scene.version, 150);
        assert!(scene.palette.from_file, "{name} should carry a palette");
    }
}

#[test]
fn a_solid_cube_meshes_to_six_quads() {
    let scene = load("cube.vox");
    assert_eq!(scene.models.len(), 1);
    assert_eq!(scene.models[0].size(), glam::UVec3::splat(4));
    assert_eq!(scene.voxel_count, 64);

    let meshes = mesh::mesh_models(&scene.models, &scene.materials);
    // Every face of a solid box has uniform shading, so each side merges into
    // a single quad: 6 quads, 12 triangles.
    assert_eq!(meshes[0].quad_count(), 6);
    assert_eq!(meshes[0].triangle_count(), 12);
}

#[test]
fn a_checkerboard_merges_nothing() {
    let scene = load("checker.vox");
    let grid = &scene.models[0];
    assert_eq!(grid.size(), glam::UVec3::splat(8));

    let meshes = mesh::mesh_models(&scene.models, &scene.materials);
    // No two exposed faces are adjacent, so the greedy pass cannot combine
    // any of them: one quad per exposed face is the floor for this mesher.
    assert_eq!(meshes[0].quad_count(), mesh::exposed_face_count(grid));
    assert_eq!(meshes[0].triangle_count(), meshes[0].quad_count() * 2);
}

#[test]
fn the_scene_graph_places_both_models() {
    let scene = load("two_models.vox");
    assert_eq!(scene.models.len(), 2);
    assert_eq!(scene.instances.len(), 2);

    // The graph is root nTRN -> nGRP -> two nTRN -> nSHP, so each shape ends
    // up with exactly its own transform: the root contributes nothing.
    let placed: Vec<(usize, IVec3)> = scene
        .instances
        .iter()
        .map(|i| (i.model_index, i.transform.translation))
        .collect();
    assert_eq!(placed, vec![(0, TWO_MODELS_T0), (1, TWO_MODELS_T1)]);

    // Names come from the nearest enclosing nTRN.
    let names: Vec<_> = scene.instances.iter().map(|i| i.name.as_deref()).collect();
    assert_eq!(names, vec![Some("left"), Some("right")]);
}

#[test]
fn transformed_models_land_where_the_bounds_say_they_do() {
    let scene = load("two_models.vox");
    let size = IVec3::splat(2);

    // A 2-wide model pivots on cell 1, so local (0,0,0) sits at t - (1,1,1).
    let first = scene.instances[0].transform;
    assert_eq!(
        first.voxel_to_world(IVec3::ZERO, size),
        TWO_MODELS_T0 - IVec3::ONE
    );
    let second = scene.instances[1].transform;
    assert_eq!(second.voxel_to_world(IVec3::ONE, size), TWO_MODELS_T1);

    // Scene bounds are the union of both boxes.
    assert_eq!(scene.bounds.min, Vec3::new(-7.0, -1.0, -1.0));
    assert_eq!(scene.bounds.max, Vec3::new(11.0, 3.0, 2.0));
    assert_eq!(scene.dimensions(), IVec3::new(18, 4, 3));
}

#[test]
fn truncating_a_fixture_never_panics() {
    let bytes = std::fs::read(fixtures_dir().join("two_models.vox")).unwrap();
    for len in 0..bytes.len() {
        // A prefix is either a complete, shorter file or a parse error --
        // never a crash.
        let _ = loader::load_bytes(&bytes[..len]);
    }
}

#[test]
fn corrupting_a_fixture_never_panics() {
    let original = std::fs::read(fixtures_dir().join("two_models.vox")).unwrap();
    // A fixed sequence so a failure is reproducible from the message alone.
    let mut rng = 0x243f_6a88_85a3_08d3u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    for _ in 0..2_000 {
        let mut bytes = original.clone();
        for _ in 0..4 {
            let at = (next() as usize) % bytes.len();
            bytes[at] = (next() & 0xff) as u8;
        }
        let _ = loader::load_bytes(&bytes);
    }
}

#[test]
fn a_directory_argument_finds_every_fixture() {
    let (paths, index) = loader::collect_vox_paths(fixtures_dir()).unwrap();
    assert_eq!(index, 0);
    assert!(paths.len() >= fixtures::FIXTURES.len());
    assert!(paths.iter().all(|p| p.extension().unwrap() == "vox"));

    // Naming one file yields the same listing, positioned on that file, so
    // `[` and `]` can page through its siblings.
    let (siblings, at) = loader::collect_vox_paths(&fixtures_dir().join("checker.vox")).unwrap();
    assert_eq!(siblings, paths);
    assert_eq!(siblings[at].file_name().unwrap(), "checker.vox");
}

/// The spec's performance floor: a 126-cubed model has to mesh in under
/// 200 ms in release. Debug and test builds are far slower, so the assertion
/// only bites when optimisations and no debug assertions are in play --
/// `cargo test --release`.
#[test]
fn a_large_model_meshes_quickly() {
    use voxview::model::VoxelGrid;

    let size = glam::UVec3::splat(126);
    let mut grid = VoxelGrid::new(size).expect("126 cubed is well within the limits");
    // A pattern that defeats trivial merging in every direction but still
    // leaves large mergeable runs: roughly what a detailed asset looks like.
    for z in 0..size.z {
        for y in 0..size.y {
            for x in 0..size.x {
                let shell = x < 2 || y < 2 || z < 2 || x > 123 || y > 123 || z > 123;
                let speckle = (x / 3 + y / 5 + z / 7) % 4 == 0;
                if shell || speckle {
                    let index = ((x * 7 + y * 13 + z * 31) % 255) as u8;
                    grid.set(glam::UVec3::new(x, y, z), Some(index));
                }
            }
        }
    }

    let started = std::time::Instant::now();
    let meshed = mesh::greedy_mesh(&grid);
    let elapsed = started.elapsed();
    assert!(!meshed.is_empty());
    println!(
        "meshed {} voxels into {} quads in {:.1} ms",
        grid.voxel_count(),
        meshed.quad_count(),
        elapsed.as_secs_f64() * 1000.0
    );

    if !cfg!(debug_assertions) {
        assert!(
            elapsed.as_millis() < 200,
            "meshing 126 cubed took {elapsed:?}, over the 200 ms budget"
        );
    }
}
