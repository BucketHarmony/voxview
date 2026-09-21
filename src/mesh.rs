//! Greedy meshing.
//!
//! For each of the six face directions we sweep the model slice by slice,
//! build a 2-D mask of the visible faces in that slice, and merge the mask
//! into as few axis-aligned rectangles as possible. That is the classic
//! greedy algorithm; the only twist here is that the merge key carries the
//! face's ambient-occlusion value as well as its palette index, so a quad is
//! only merged across faces that shade identically.
//!
//! Meshes are built in the model's own local space, `0..size`. Placing them in
//! the world is the model matrix's job (see [`crate::scene::VoxTransform`]).

use crate::model::VoxelGrid;
use glam::{IVec3, Vec3};

/// Face directions, in the order their indices are packed into a vertex.
pub const FACE_NORMALS: [IVec3; 6] = [
    IVec3::new(1, 0, 0),
    IVec3::new(-1, 0, 0),
    IVec3::new(0, 1, 0),
    IVec3::new(0, -1, 0),
    IVec3::new(0, 0, 1),
    IVec3::new(0, 0, -1),
];

/// A mesh vertex.
///
/// Colour is not stored here: the vertex carries a *palette index* and the
/// shader looks the colour up in a 256-entry uniform. See `README.md` for why.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    /// Bits 0-7 palette index, bits 8-10 face index, bits 11-18 AO (0-255).
    pub packed: u32,
}

impl Vertex {
    pub fn new(position: Vec3, palette_index: u8, face: usize, ao: u8) -> Vertex {
        Vertex {
            position: position.to_array(),
            packed: palette_index as u32 | ((face as u32) << 8) | ((ao as u32) << 11),
        }
    }

    pub fn palette_index(self) -> u8 {
        (self.packed & 0xff) as u8
    }

    pub fn face(self) -> usize {
        ((self.packed >> 8) & 0b111) as usize
    }

    pub fn ao(self) -> u8 {
        ((self.packed >> 11) & 0xff) as u8
    }
}

/// Triangles for one model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl Mesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Number of merged rectangles; four vertices each.
    pub fn quad_count(&self) -> usize {
        self.vertices.len() / 4
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    fn push_quad(&mut self, corners: [Vec3; 4], palette_index: u8, face: usize, ao: u8) {
        let base = self.vertices.len() as u32;
        for c in corners {
            self.vertices.push(Vertex::new(c, palette_index, face, ao));
        }
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// What makes two faces mergeable: same colour, same shading.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FaceKey {
    palette_index: u8,
    ao: u8,
}

/// Greedily mesh one model.
pub fn greedy_mesh(grid: &VoxelGrid) -> Mesh {
    let mut mesh = Mesh::default();
    if grid.is_empty() {
        return mesh;
    }
    let size = grid.size().as_ivec3();
    let dims = size.to_array();
    let largest_slice = (dims[0] * dims[1])
        .max(dims[1] * dims[2])
        .max(dims[2] * dims[0]);
    let mut mask: Vec<Option<FaceKey>> = vec![None; largest_slice as usize];

    for axis in 0..3usize {
        // A right-handed basis: u cross v is the +axis direction, which is what
        // makes the winding below come out correct without special cases.
        let u = (axis + 1) % 3;
        let v = (axis + 2) % 3;
        let (du, dv) = (unit(u), unit(v));
        let (su, sv) = (dims[u], dims[v]);

        for (dir_index, dir) in [1i32, -1].into_iter().enumerate() {
            let face = axis * 2 + dir_index;
            let normal = FACE_NORMALS[face];

            for slice in 0..dims[axis] {
                mask[..(su * sv) as usize].fill(None);
                for b in 0..sv {
                    for a in 0..su {
                        let mut p = IVec3::ZERO;
                        p[axis] = slice;
                        p[u] = a;
                        p[v] = b;
                        let Some(palette_index) = grid.get_i(p) else {
                            continue;
                        };
                        if grid.is_solid(p + normal) {
                            continue; // hidden by its neighbour
                        }
                        mask[(a + b * su) as usize] = Some(FaceKey {
                            palette_index,
                            ao: face_ao(grid, p, normal, du, dv),
                        });
                    }
                }

                // The face plane sits on the far side of the voxel for a
                // positive direction and on the near side for a negative one.
                let plane = if dir > 0 { slice + 1 } else { slice };
                merge_mask(&mut mesh, &mut mask, su, sv, axis, u, v, plane, face, dir);
            }
        }
    }
    mesh
}

/// Mesh every model in a scene. Models are meshed once even when a scene
/// instances them several times.
pub fn mesh_models(models: &[VoxelGrid]) -> Vec<Mesh> {
    models.iter().map(greedy_mesh).collect()
}

fn unit(axis: usize) -> IVec3 {
    let mut v = IVec3::ZERO;
    v[axis] = 1;
    v
}

/// Merge the mask into rectangles and emit one quad per rectangle.
#[allow(clippy::too_many_arguments)]
fn merge_mask(
    mesh: &mut Mesh,
    mask: &mut [Option<FaceKey>],
    su: i32,
    sv: i32,
    axis: usize,
    u: usize,
    v: usize,
    plane: i32,
    face: usize,
    dir: i32,
) {
    let mut b = 0;
    while b < sv {
        let mut a = 0;
        while a < su {
            let Some(key) = mask[(a + b * su) as usize] else {
                a += 1;
                continue;
            };

            // Grow along u, then grow the whole run along v.
            let mut w = 1;
            while a + w < su && mask[(a + w + b * su) as usize] == Some(key) {
                w += 1;
            }
            let mut h = 1;
            'grow: while b + h < sv {
                for i in 0..w {
                    if mask[(a + i + (b + h) * su) as usize] != Some(key) {
                        break 'grow;
                    }
                }
                h += 1;
            }

            for j in 0..h {
                for i in 0..w {
                    mask[(a + i + (b + j) * su) as usize] = None;
                }
            }

            let corner = |ua: i32, vb: i32| {
                let mut p = IVec3::ZERO;
                p[axis] = plane;
                p[u] = ua;
                p[v] = vb;
                p.as_vec3()
            };
            let p00 = corner(a, b);
            let p10 = corner(a + w, b);
            let p11 = corner(a + w, b + h);
            let p01 = corner(a, b + h);
            // Counter-clockwise seen from outside the face.
            let corners = if dir > 0 {
                [p00, p10, p11, p01]
            } else {
                [p00, p01, p11, p10]
            };
            mesh.push_quad(corners, key.palette_index, face, key.ao);

            a += w;
        }
        b += 1;
    }
}

/// Ambient occlusion for one face, as a single 0-255 value.
///
/// Each of the face's four corners gets the usual three-neighbour AO term, and
/// the four are averaged. A per-face value (rather than per-vertex) is what
/// lets merged quads stay merged: the value goes into the merge key, so faces
/// only join up when they are equally occluded.
fn face_ao(grid: &VoxelGrid, p: IVec3, normal: IVec3, du: IVec3, dv: IVec3) -> u8 {
    let front = p + normal;
    // Sample the 3x3 neighbourhood of the cell in front of the face.
    let mut n = [[false; 3]; 3];
    for (i, item) in n.iter_mut().enumerate() {
        for (j, cell) in item.iter_mut().enumerate() {
            *cell = grid.is_solid(front + du * (i as i32 - 1) + dv * (j as i32 - 1));
        }
    }
    let mut total = 0u32;
    for &su in &[-1i32, 1] {
        for &sv in &[-1i32, 1] {
            let side_u = n[(su + 1) as usize][1];
            let side_v = n[1][(sv + 1) as usize];
            let diagonal = n[(su + 1) as usize][(sv + 1) as usize];
            total += if side_u && side_v {
                0
            } else {
                3 - (side_u as u32 + side_v as u32 + diagonal as u32)
            };
        }
    }
    // total is 0..=12; map onto 0..=255 with no rounding drift.
    (total * 255 / 12) as u8
}

/// Faces that would be drawn with no merging at all. Only used by tests, as a
/// check on the greedy pass that does not share any of its code.
pub fn exposed_face_count(grid: &VoxelGrid) -> usize {
    let size = grid.size().as_ivec3();
    let mut count = 0;
    for z in 0..size.z {
        for y in 0..size.y {
            for x in 0..size.x {
                let p = IVec3::new(x, y, z);
                if !grid.is_solid(p) {
                    continue;
                }
                count += FACE_NORMALS
                    .iter()
                    .filter(|n| !grid.is_solid(p + **n))
                    .count();
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::UVec3;

    fn grid_from(size: UVec3, filled: &[(u32, u32, u32)]) -> VoxelGrid {
        let mut g = VoxelGrid::new(size).unwrap();
        for &(x, y, z) in filled {
            g.set(UVec3::new(x, y, z), Some(7));
        }
        g
    }

    fn solid(size: UVec3) -> VoxelGrid {
        let mut g = VoxelGrid::new(size).unwrap();
        for z in 0..size.z {
            for y in 0..size.y {
                for x in 0..size.x {
                    g.set(UVec3::new(x, y, z), Some(7));
                }
            }
        }
        g
    }

    #[test]
    fn empty_grid_makes_no_triangles() {
        let g = VoxelGrid::new(UVec3::new(4, 4, 4)).unwrap();
        assert!(greedy_mesh(&g).is_empty());
    }

    #[test]
    fn single_voxel_is_twelve_triangles() {
        let g = solid(UVec3::splat(1));
        let mesh = greedy_mesh(&g);
        assert_eq!(mesh.triangle_count(), 12);
        assert_eq!(mesh.quad_count(), 6);
        assert_eq!(exposed_face_count(&g), 6);
    }

    #[test]
    fn solid_two_cube_merges_to_twelve_triangles() {
        // 24 exposed faces, but each side is a flat 2x2 patch that is equally
        // lit, so greedy meshing collapses each side to a single quad.
        let g = solid(UVec3::splat(2));
        let mesh = greedy_mesh(&g);
        assert_eq!(exposed_face_count(&g), 24);
        assert_eq!(mesh.triangle_count(), 12);
        assert_eq!(mesh.quad_count(), 6);
    }

    #[test]
    fn bigger_solid_cubes_also_collapse_to_six_quads() {
        for n in [3u32, 4, 8, 16] {
            let mesh = greedy_mesh(&solid(UVec3::splat(n)));
            assert_eq!(mesh.quad_count(), 6, "solid {n}-cube");
        }
    }

    #[test]
    fn l_shape_face_count() {
        // Three voxels in the z = 0 plane:
        //   . .        y = 1:  X .
        //   X X        y = 0:  X X
        let g = grid_from(UVec3::new(2, 2, 1), &[(0, 0, 0), (1, 0, 0), (0, 1, 0)]);
        assert_eq!(exposed_face_count(&g), 14);

        // Merging, direction by direction:
        //   -Z and +Z: an L in the xy plane merges into 2 quads each
        //   -X: the two voxels at x = 0 share a plane and merge      -> 1
        //   +X: exposed faces sit in different planes (x = 1, x = 2) -> 2
        //   -Y: the two voxels at y = 0 share a plane and merge      -> 1
        //   +Y: exposed faces sit in different planes (y = 1, y = 2) -> 2
        let mesh = greedy_mesh(&g);
        assert_eq!(mesh.quad_count(), 10);
        assert_eq!(mesh.triangle_count(), 20);
    }

    #[test]
    fn checkerboard_cannot_merge_anything() {
        let mut g = VoxelGrid::new(UVec3::splat(4)).unwrap();
        let mut filled = 0;
        for z in 0..4 {
            for y in 0..4 {
                for x in 0..4 {
                    if (x + y + z) % 2 == 0 {
                        g.set(UVec3::new(x, y, z), Some(7));
                        filled += 1;
                    }
                }
            }
        }
        // No two solid cells touch, so every voxel shows all six faces and no
        // two faces are coplanar neighbours.
        let mesh = greedy_mesh(&g);
        assert_eq!(exposed_face_count(&g), filled * 6);
        assert_eq!(mesh.quad_count(), filled * 6);
    }

    #[test]
    fn faces_point_outwards() {
        // Every triangle of a convex solid must face away from the centre.
        let g = solid(UVec3::splat(3));
        let mesh = greedy_mesh(&g);
        let centre = Vec3::splat(1.5);
        for tri in mesh.indices.as_chunks::<3>().0 {
            let a = Vec3::from(mesh.vertices[tri[0] as usize].position);
            let b = Vec3::from(mesh.vertices[tri[1] as usize].position);
            let c = Vec3::from(mesh.vertices[tri[2] as usize].position);
            let winding_normal = (b - a).cross(c - a);
            let declared = FACE_NORMALS[mesh.vertices[tri[0] as usize].face()].as_vec3();
            assert!(
                winding_normal.dot(declared) > 0.0,
                "winding disagrees with the declared face normal"
            );
            assert!(
                winding_normal.dot(a - centre) > 0.0,
                "triangle faces inwards"
            );
        }
    }

    #[test]
    fn vertex_packing_round_trips() {
        for &ao in &[0u8, 1, 128, 255] {
            for face in 0..6 {
                let v = Vertex::new(Vec3::ZERO, 200, face, ao);
                assert_eq!(v.palette_index(), 200);
                assert_eq!(v.face(), face);
                assert_eq!(v.ao(), ao);
            }
        }
    }

    #[test]
    fn ao_darkens_a_concave_corner() {
        // Two voxels meeting at a right angle: the +X face of the lower one is
        // partly shadowed by the upper one, so it must not merge with a face
        // that is fully lit.
        let g = grid_from(UVec3::new(2, 1, 2), &[(0, 0, 0), (0, 0, 1), (1, 0, 1)]);
        let mesh = greedy_mesh(&g);
        let lit = mesh.vertices.iter().filter(|v| v.ao() == 255).count();
        assert!(lit > 0, "some faces should be fully lit");
        assert!(
            mesh.vertices.iter().any(|v| v.ao() < 255),
            "the concave corner should be occluded"
        );
    }

    #[test]
    fn indices_are_in_range() {
        let g = solid(UVec3::new(5, 3, 7));
        let mesh = greedy_mesh(&g);
        assert!(!mesh.is_empty());
        for &i in &mesh.indices {
            assert!((i as usize) < mesh.vertices.len());
        }
    }
}
