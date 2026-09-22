//! Turning bytes on disk into something the renderer can chew on.

use crate::model::VoxelGrid;
use crate::palette::Palette;
use crate::scene::{Bounds, ModelInstance, flatten};
use anyhow::{Context, Result, anyhow};
use glam::{IVec3, Vec3};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

/// A parsed `.vox` file, flattened and ready to mesh.
#[derive(Clone, Debug)]
pub struct VoxScene {
    /// File format version from the header (150 for current MagicaVoxel).
    pub version: u32,
    /// Every model in the file, in file order.
    pub models: Vec<VoxelGrid>,
    /// One entry per visible placement of a model.
    pub instances: Vec<ModelInstance>,
    pub palette: Palette,
    /// Number of `MATL` chunks; read but not rendered in v1.
    pub material_count: usize,
    /// Total occupied cells across every *instanced* model.
    pub voxel_count: usize,
    /// World-space bounds of all instances, in voxel units.
    pub bounds: Bounds,
}

impl VoxScene {
    /// Dimensions of the whole scene in voxels, rounded out to whole cells.
    pub fn dimensions(&self) -> IVec3 {
        self.bounds.extent().round().as_ivec3()
    }
}

/// Read and parse a `.vox` file.
pub fn load_file(path: &Path) -> Result<VoxScene> {
    let bytes =
        std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    load_bytes(&bytes).with_context(|| format!("could not parse {}", path.display()))
}

/// Parse `.vox` bytes.
///
/// `dot_vox` documents that it does not panic, but this viewer's contract is
/// that *no* input crashes it, so the parse is wrapped. The guard costs
/// nothing on the success path.
pub fn load_bytes(bytes: &[u8]) -> Result<VoxScene> {
    let parsed = catch_unwind(AssertUnwindSafe(|| dot_vox::load_bytes(bytes)))
        .map_err(|_| anyhow!("the .vox parser panicked on this file"))?
        .map_err(|e| anyhow!("{e}"))?;

    let mut models = Vec::with_capacity(parsed.models.len());
    for (i, m) in parsed.models.iter().enumerate() {
        let grid = VoxelGrid::from_dot_vox(m).ok_or_else(|| {
            anyhow!(
                "model {i} declares an implausible size of {}x{}x{}",
                m.size.x,
                m.size.y,
                m.size.z
            )
        })?;
        models.push(grid);
    }

    let palette = if parsed.palette.is_empty() {
        Palette::magicavoxel_default()
    } else {
        Palette::from_file_colors(&parsed.palette)
    };

    let hidden_layers: Vec<bool> = parsed.layers.iter().map(|l| l.hidden()).collect();
    let instances = flatten(&parsed.scenes, &hidden_layers, models.len());

    let mut voxel_count = 0usize;
    let mut bounds: Option<Bounds> = None;
    for inst in &instances {
        let grid = &models[inst.model_index];
        voxel_count += grid.voxel_count();
        let size = grid.size().as_ivec3();
        let b = instance_bounds(inst, size);
        bounds = Some(match bounds {
            Some(prev) => prev.union(b),
            None => b,
        });
    }

    Ok(VoxScene {
        version: parsed.version,
        models,
        instances,
        palette,
        material_count: parsed.materials.len(),
        voxel_count,
        bounds: bounds.unwrap_or_default(),
    })
}

/// World-space bounds of one instance's full model box.
fn instance_bounds(inst: &ModelInstance, size: IVec3) -> Bounds {
    // A signed permutation only ever maps the box's corners onto each other,
    // so transforming the two extreme corners and taking min/max is exact.
    let a = inst.transform.model_matrix(size) * Vec3::ZERO.extend(1.0);
    let b = inst.transform.model_matrix(size) * size.as_vec3().extend(1.0);
    Bounds {
        min: a.truncate().min(b.truncate()),
        max: a.truncate().max(b.truncate()),
    }
}

/// How deep the recursive walk will go before it gives up on a branch.
///
/// Veloren's deepest asset path is six directories; 24 is far past anything
/// real and stops a symlink loop the `is_symlink` check somehow missed.
const MAX_WALK_DEPTH: usize = 24;

/// Ceiling on files returned by one walk. Veloren ships ~4,800; this is two
/// orders of magnitude of headroom and still bounds memory on a stray root.
const MAX_WALK_FILES: usize = 200_000;

/// The result of walking a directory tree for `.vox` files.
#[derive(Clone, Debug, Default)]
pub struct Walk {
    /// Every `.vox` file found, sorted by path.
    pub files: Vec<std::path::PathBuf>,
    /// Set when [`MAX_WALK_FILES`] or [`MAX_WALK_DEPTH`] cut the walk short.
    pub truncated: bool,
    /// Directories that could not be read, with the reason. Listed rather than
    /// returned as an error: one unreadable folder should not lose the rest.
    pub skipped: Vec<(std::path::PathBuf, String)>,
}

/// Collect the `.vox` files a CLI argument refers to.
///
/// A directory yields its whole subtree, so pointing at `voxel/sprite` covers
/// all 950 files under it rather than the handful sitting directly there. A
/// *file* yields only its own directory's listing: paging with `[` and `]`
/// should stay instant no matter how large the tree above it is, and the
/// library browser does its own recursive scan off the main thread.
pub fn collect_vox_paths(arg: &Path) -> Result<(Vec<std::path::PathBuf>, usize)> {
    if arg.is_dir() {
        let walk = walk_vox_dir(arg);
        if walk.files.is_empty() {
            return Err(anyhow!("no .vox files under {}", arg.display()));
        }
        Ok((walk.files, 0))
    } else {
        if !arg.exists() {
            return Err(anyhow!("{} does not exist", arg.display()));
        }
        let canonical = std::fs::canonicalize(arg).unwrap_or_else(|_| arg.to_path_buf());
        let siblings = arg
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .and_then(|dir| list_vox_dir(dir).ok())
            .unwrap_or_default();
        let index = siblings
            .iter()
            .position(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == canonical);
        match index {
            Some(i) => Ok((siblings, i)),
            // The named file is not a `.vox` by extension, or lives somewhere
            // we could not list: show just it.
            None => Ok((vec![arg.to_path_buf()], 0)),
        }
    }
}

/// The directory a CLI argument should be browsed from: itself, or the file's
/// parent. Used as the root of the library's recursive scan.
pub fn browse_root(arg: &Path) -> std::path::PathBuf {
    if arg.is_dir() {
        arg.to_path_buf()
    } else {
        match arg.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => std::path::PathBuf::from("."),
        }
    }
}

/// Recursively find every `.vox` file under `root`.
///
/// Never fails: an unreadable directory lands in [`Walk::skipped`] and the walk
/// carries on. The traversal keeps its own stack rather than recursing, so a
/// pathological tree cannot blow the real one.
pub fn walk_vox_dir(root: &Path) -> Walk {
    let mut walk = Walk::default();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    // Depth-first, but visiting children in reverse so that popping yields
    // them in name order -- a cold scan then fills the browser top to bottom.
    while let Some((dir, depth)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                walk.skipped.push((dir, e.to_string()));
                continue;
            }
        };

        let mut children = Vec::new();
        for entry in entries.flatten() {
            // `file_type` from `read_dir` does not follow links, so a symlinked
            // directory is visible as one and skipped -- that is the whole
            // cycle defence. Windows junctions report as symlinks here too.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if depth + 1 > MAX_WALK_DEPTH {
                    walk.truncated = true;
                } else {
                    children.push(path);
                }
            } else if kind.is_file() && is_vox(&path) {
                if walk.files.len() >= MAX_WALK_FILES {
                    walk.truncated = true;
                    return finish(walk);
                }
                walk.files.push(path);
            }
        }

        children.sort();
        for child in children.into_iter().rev() {
            stack.push((child, depth + 1));
        }
    }
    finish(walk)
}

fn finish(mut walk: Walk) -> Walk {
    walk.files.sort();
    walk.skipped.sort();
    walk
}

fn is_vox(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("vox"))
}

/// List the `.vox` files directly inside `dir`, sorted by name.
pub fn list_vox_dir(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("could not list {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_vox(p))
        .collect();
    files.sort();
    Ok(files)
}

// EXTENSION: Veloren RON manifests describe multi-part assemblies (a body made
// of head/chest/limb `.vox` files, each with its own offset). A loader for
// those would parse the manifest, call `load_file` per part, and merge the
// results into one `VoxScene` by appending each part's models and pushing a
// `ModelInstance` whose `transform.translation` carries the manifest offset --
// no change to the mesher or renderer is needed, because they already handle
// many models at many transforms.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(load_bytes(b"not a vox file at all").is_err());
        assert!(load_bytes(&[]).is_err());
        assert!(load_bytes(b"VOX ").is_err());
    }

    #[test]
    fn truncated_header_is_an_error() {
        let mut bytes = b"VOX ".to_vec();
        bytes.extend_from_slice(&150u32.to_le_bytes());
        assert!(load_bytes(&bytes).is_err());
    }

    /// A unique scratch directory, removed by the test that made it.
    fn scratch(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "voxview-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    #[test]
    fn walk_finds_nested_files_and_ignores_other_extensions() {
        let root = scratch("walk");
        touch(&root.join("top.vox"));
        touch(&root.join("notes.txt"));
        touch(&root.join("npc/wolf/male/head.vox"));
        touch(&root.join("npc/wolf/male/body.VOX"));
        touch(&root.join("npc/wolf/wolf_manifest.ron"));

        let walk = walk_vox_dir(&root);
        let names: Vec<_> = walk
            .files
            .iter()
            .map(|p| {
                p.strip_prefix(&root)
                    .unwrap()
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .collect();

        assert_eq!(
            names,
            [
                "npc/wolf/male/body.VOX",
                "npc/wolf/male/head.vox",
                "top.vox"
            ]
        );
        assert!(!walk.truncated);
        assert!(walk.skipped.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn walk_of_a_missing_directory_reports_it_instead_of_failing() {
        let walk = walk_vox_dir(Path::new("no/such/directory/anywhere"));
        assert!(walk.files.is_empty());
        assert_eq!(walk.skipped.len(), 1);
    }

    #[test]
    fn a_directory_argument_covers_the_whole_subtree() {
        let root = scratch("arg");
        touch(&root.join("a/deep/one.vox"));
        touch(&root.join("b/two.vox"));

        let (files, index) = collect_vox_paths(&root).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(index, 0);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_file_argument_pages_its_own_directory_only() {
        let root = scratch("file");
        touch(&root.join("one.vox"));
        touch(&root.join("two.vox"));
        touch(&root.join("sub/three.vox"));

        let (files, index) = collect_vox_paths(&root.join("two.vox")).unwrap();
        assert_eq!(files.len(), 2, "the subdirectory must not be pulled in");
        assert_eq!(index, 1);
        assert_eq!(browse_root(&root.join("two.vox")), root);
        std::fs::remove_dir_all(&root).ok();
    }
}
