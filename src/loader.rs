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
    let bytes = std::fs::read(path)
        .with_context(|| format!("could not read {}", path.display()))?;
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

/// Collect the `.vox` files a CLI argument refers to.
///
/// A file yields its own directory's listing so `[` and `]` can page through
/// siblings; a directory yields its `.vox` children, sorted by name.
pub fn collect_vox_paths(arg: &Path) -> Result<(Vec<std::path::PathBuf>, usize)> {
    if arg.is_dir() {
        let files = list_vox_dir(arg)?;
        if files.is_empty() {
            return Err(anyhow!("no .vox files in {}", arg.display()));
        }
        Ok((files, 0))
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
        let index = siblings.iter().position(|p| {
            std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == canonical
        });
        match index {
            Some(i) => Ok((siblings, i)),
            // The named file is not a `.vox` by extension, or lives somewhere
            // we could not list: show just it.
            None => Ok((vec![arg.to_path_buf()], 0)),
        }
    }
}

fn list_vox_dir(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("could not list {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("vox"))
        })
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
}
