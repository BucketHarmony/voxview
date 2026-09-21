//! A dense voxel grid: the form the mesher and the AO sampler want.

use glam::{IVec3, UVec3};

/// Largest number of voxel cells we will allocate for a single model.
///
/// MagicaVoxel itself tops out at 256 per axis, but extended writers go higher.
/// The cap exists so a corrupt `SIZE` chunk turns into an error instead of a
/// multi-gigabyte allocation.
pub const MAX_CELLS: u64 = 256 * 1024 * 1024;
/// Largest extent we accept along any single axis.
pub const MAX_EXTENT: u32 = 2048;

/// A model's occupancy, stored as `palette_index + 1` with `0` meaning empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoxelGrid {
    size: UVec3,
    /// `x + y * size.x + z * size.x * size.y`, holding `palette_index + 1`.
    /// `u16` rather than `u8` so that palette index 255 stays distinct from
    /// the `0` that means "empty".
    cells: Vec<u16>,
    voxel_count: usize,
}

impl VoxelGrid {
    /// An all-empty grid of the given dimensions.
    ///
    /// Returns `None` when the dimensions are implausible (see [`MAX_CELLS`]).
    pub fn new(size: UVec3) -> Option<Self> {
        if size.x > MAX_EXTENT || size.y > MAX_EXTENT || size.z > MAX_EXTENT {
            return None;
        }
        let cells = (size.x as u64) * (size.y as u64) * (size.z as u64);
        if cells > MAX_CELLS {
            return None;
        }
        Some(VoxelGrid {
            size,
            cells: vec![0u16; cells as usize],
            voxel_count: 0,
        })
    }

    /// Build a grid from `dot_vox`'s model representation.
    ///
    /// Out-of-range voxels (which malformed files do contain) are dropped
    /// rather than treated as an error.
    pub fn from_dot_vox(model: &dot_vox::Model) -> Option<Self> {
        let size = UVec3::new(model.size.x, model.size.y, model.size.z);
        let mut grid = VoxelGrid::new(size)?;
        for v in &model.voxels {
            grid.set(UVec3::new(v.x as u32, v.y as u32, v.z as u32), Some(v.i));
        }
        Some(grid)
    }

    pub fn size(&self) -> UVec3 {
        self.size
    }

    /// Number of occupied cells.
    pub fn voxel_count(&self) -> usize {
        self.voxel_count
    }

    pub fn is_empty(&self) -> bool {
        self.voxel_count == 0
    }

    #[inline]
    fn index(&self, p: UVec3) -> Option<usize> {
        if p.x >= self.size.x || p.y >= self.size.y || p.z >= self.size.z {
            return None;
        }
        Some((p.x + p.y * self.size.x + p.z * self.size.x * self.size.y) as usize)
    }

    /// Set or clear a cell. Coordinates outside the grid are ignored.
    pub fn set(&mut self, p: UVec3, palette_index: Option<u8>) {
        let Some(i) = self.index(p) else { return };
        let was_solid = self.cells[i] != 0;
        let now = palette_index.map_or(0u16, |c| c as u16 + 1);
        self.cells[i] = now;
        match (was_solid, now != 0) {
            (false, true) => self.voxel_count += 1,
            (true, false) => self.voxel_count -= 1,
            _ => {}
        }
    }

    /// Palette index at `p`, or `None` if the cell is empty or out of bounds.
    #[inline]
    pub fn get(&self, p: UVec3) -> Option<u8> {
        let i = self.index(p)?;
        match self.cells[i] {
            0 => None,
            v => Some((v - 1) as u8),
        }
    }

    /// Like [`VoxelGrid::get`] but takes signed coordinates; anything outside
    /// the grid reads as empty. This is what the mesher and AO sampler use.
    #[inline]
    pub fn get_i(&self, p: IVec3) -> Option<u8> {
        if p.x < 0 || p.y < 0 || p.z < 0 {
            return None;
        }
        self.get(p.as_uvec3())
    }

    #[inline]
    pub fn is_solid(&self, p: IVec3) -> bool {
        self.get_i(p).is_some()
    }
}
