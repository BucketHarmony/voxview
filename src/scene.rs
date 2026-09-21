//! Scene-graph flattening.
//!
//! MagicaVoxel 0.99+ stores a scene graph of `nTRN` (transform), `nGRP`
//! (group) and `nSHP` (shape) chunks. Flattening it gives one *instance* per
//! rendered model, each with an integer rigid transform.
//!
//! Coordinates are MagicaVoxel's own: right-handed, Z up, one unit per voxel.

use dot_vox::Dict;
use glam::{IVec3, Mat3, Mat4, Vec3};

/// A signed permutation matrix -- the only rotation `.vox` can express.
///
/// `src[i]` is the input axis that feeds output axis `i`, and `neg[i]` its
/// sign. Kept as integers so transform composition is exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rot3 {
    src: [u8; 3],
    neg: [bool; 3],
}

impl Default for Rot3 {
    fn default() -> Self {
        Rot3::IDENTITY
    }
}

impl Rot3 {
    pub const IDENTITY: Rot3 = Rot3 {
        src: [0, 1, 2],
        neg: [false; 3],
    };

    /// Decode MagicaVoxel's `_r` byte.
    ///
    /// Bits 0-1 and 2-3 name the non-zero column of rows 0 and 1; row 2 gets
    /// whichever column is left. Bits 4-6 are the per-row signs. Returns
    /// `None` for the bit patterns that do not describe a permutation --
    /// `dot_vox`'s own `Rotation::from_byte` asserts on those instead.
    pub fn from_byte(byte: u8) -> Option<Rot3> {
        let r0 = byte & 0b11;
        let r1 = (byte >> 2) & 0b11;
        if r0 > 2 || r1 > 2 || r0 == r1 {
            return None;
        }
        let r2 = 3 - r0 - r1;
        Some(Rot3 {
            src: [r0, r1, r2],
            neg: [
                byte & (1 << 4) != 0,
                byte & (1 << 5) != 0,
                byte & (1 << 6) != 0,
            ],
        })
    }

    /// Apply to an integer vector.
    #[inline]
    pub fn apply(self, v: IVec3) -> IVec3 {
        let a = v.to_array();
        let mut out = [0i32; 3];
        for i in 0..3 {
            let c = a[self.src[i] as usize];
            out[i] = if self.neg[i] { -c } else { c };
        }
        IVec3::from(out)
    }

    /// `self * rhs`: the rotation that applies `rhs` first, then `self`.
    pub fn compose(self, rhs: Rot3) -> Rot3 {
        let mut src = [0u8; 3];
        let mut neg = [false; 3];
        for i in 0..3 {
            let k = self.src[i] as usize;
            src[i] = rhs.src[k];
            neg[i] = self.neg[i] ^ rhs.neg[k];
        }
        Rot3 { src, neg }
    }

    /// One per output axis this rotation negates.
    ///
    /// A negated axis flips a unit cube onto the far side of its own origin,
    /// so a box built in `0..size` needs shifting back by one cell along that
    /// axis to line up with where the integer placement puts it.
    fn negation_offset(self) -> Vec3 {
        Vec3::new(
            self.neg[0] as u8 as f32,
            self.neg[1] as u8 as f32,
            self.neg[2] as u8 as f32,
        )
    }

    /// The same matrix as floats, for building a model matrix.
    pub fn to_mat3(self) -> Mat3 {
        let mut m = Mat3::ZERO;
        for i in 0..3 {
            let s = if self.neg[i] { -1.0 } else { 1.0 };
            // Mat3 is column-major, so this writes element (row i, col src[i]).
            m.col_mut(self.src[i] as usize)[i] = s;
        }
        m
    }
}

/// A rigid, axis-aligned transform in voxel units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct VoxTransform {
    pub rotation: Rot3,
    pub translation: IVec3,
}

impl VoxTransform {
    pub const IDENTITY: VoxTransform = VoxTransform {
        rotation: Rot3::IDENTITY,
        translation: IVec3::ZERO,
    };

    /// `self` applied after `child`.
    pub fn compose(self, child: VoxTransform) -> VoxTransform {
        VoxTransform {
            rotation: self.rotation.compose(child.rotation),
            translation: self.rotation.apply(child.translation) + self.translation,
        }
    }

    /// World-space integer coordinate of the voxel at local coordinate `v` in
    /// a model of dimensions `size`.
    ///
    /// `_t` gives the world position of the model's pivot, and MagicaVoxel's
    /// pivot is the *cell* `size / 2` in integer division -- not the
    /// geometric centre. For even sizes the two agree; for odd sizes they
    /// differ by half a cell, and using the geometric centre would put the
    /// model half a voxel off the world grid. Keeping the whole calculation
    /// in integers makes that impossible to get subtly wrong.
    pub fn voxel_to_world(self, v: IVec3, size: IVec3) -> IVec3 {
        self.rotation.apply(v - size / 2) + self.translation
    }

    /// Matrix that maps a model's *local* mesh (built in `0..size`) into world
    /// space, matching [`VoxTransform::voxel_to_world`] exactly.
    pub fn model_matrix(self, size: IVec3) -> Mat4 {
        let pivot = (size / 2).as_vec3();
        let rot = self.rotation.to_mat3();
        Mat4::from_translation(self.translation.as_vec3() + self.rotation.negation_offset())
            * Mat4::from_mat3(rot)
            * Mat4::from_translation(-pivot)
    }
}

/// One model placed in the world.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelInstance {
    /// Index into [`crate::loader::VoxScene::models`].
    pub model_index: usize,
    pub transform: VoxTransform,
    /// `_name` of the nearest enclosing transform node, when it has one.
    pub name: Option<String>,
}

/// Depth limit for scene traversal; also breaks cycles in corrupt files.
const MAX_DEPTH: u32 = 256;

/// Walk `nodes` from the root and produce one instance per visible shape.
///
/// `hidden_layers` is consulted for `nTRN` nodes that name a hidden layer.
/// Nodes flagged `_hidden` are skipped, matching what MagicaVoxel displays.
pub fn flatten(
    nodes: &[dot_vox::SceneNode],
    hidden_layers: &[bool],
    model_count: usize,
) -> Vec<ModelInstance> {
    let mut out = Vec::new();
    if nodes.is_empty() {
        // Pre-0.99 files have no scene graph: every model sits at the origin.
        out.extend((0..model_count).map(|model_index| ModelInstance {
            model_index,
            transform: VoxTransform::IDENTITY,
            name: None,
        }));
        return out;
    }
    let mut visiting = Vec::new();
    visit(
        nodes,
        hidden_layers,
        model_count,
        0,
        VoxTransform::IDENTITY,
        None,
        0,
        &mut visiting,
        &mut out,
    );
    out
}

#[allow(clippy::too_many_arguments)]
fn visit(
    nodes: &[dot_vox::SceneNode],
    hidden_layers: &[bool],
    model_count: usize,
    node_id: u32,
    parent: VoxTransform,
    name: Option<&str>,
    depth: u32,
    visiting: &mut Vec<u32>,
    out: &mut Vec<ModelInstance>,
) {
    if depth > MAX_DEPTH || visiting.contains(&node_id) {
        return;
    }
    let Some(node) = nodes.get(node_id as usize) else {
        return;
    };
    visiting.push(node_id);
    match node {
        dot_vox::SceneNode::Transform {
            attributes,
            frames,
            child,
            layer_id,
        } => {
            let hidden = attributes.get("_hidden").map(String::as_str) == Some("1")
                || hidden_layers
                    .get(*layer_id as usize)
                    .copied()
                    .unwrap_or(false);
            if !hidden {
                let local = frame_transform(frames.first());
                let name = attributes.get("_name").map(String::as_str).or(name);
                visit(
                    nodes,
                    hidden_layers,
                    model_count,
                    *child,
                    parent.compose(local),
                    name,
                    depth + 1,
                    visiting,
                    out,
                );
            }
        }
        dot_vox::SceneNode::Group { children, .. } => {
            for child in children {
                visit(
                    nodes,
                    hidden_layers,
                    model_count,
                    *child,
                    parent,
                    name,
                    depth + 1,
                    visiting,
                    out,
                );
            }
        }
        dot_vox::SceneNode::Shape { models, .. } => {
            // Multiple entries here are animation keyframes; render the first.
            if let Some(shape) = models.first()
                && (shape.model_id as usize) < model_count
            {
                out.push(ModelInstance {
                    model_index: shape.model_id as usize,
                    transform: parent,
                    name: name.map(str::to_owned),
                });
            }
        }
    }
    visiting.pop();
}

/// Read `_r` and `_t` out of a keyframe dictionary.
///
/// We parse the raw attributes rather than calling `Frame::orientation()`,
/// because `dot_vox`'s `Rotation::from_byte` asserts on rotation bytes that
/// are not permutations -- a malformed file would abort the process.
fn frame_transform(frame: Option<&dot_vox::Frame>) -> VoxTransform {
    let Some(frame) = frame else {
        return VoxTransform::IDENTITY;
    };
    let rotation = frame
        .attributes
        .get("_r")
        .and_then(|s| s.trim().parse::<u8>().ok())
        .and_then(Rot3::from_byte)
        .unwrap_or(Rot3::IDENTITY);
    let translation = frame
        .attributes
        .get("_t")
        .and_then(|s| parse_ivec3(s))
        .unwrap_or(IVec3::ZERO);
    VoxTransform {
        rotation,
        translation,
    }
}

fn parse_ivec3(s: &str) -> Option<IVec3> {
    let mut it = s.split_whitespace();
    let x = it.next()?.parse().ok()?;
    let y = it.next()?.parse().ok()?;
    let z = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some(IVec3::new(x, y, z))
}

/// Build a `_t`/`_r` keyframe dictionary; used to synthesise test fixtures.
pub fn frame_dict(translation: IVec3, rotation_byte: Option<u8>) -> Dict {
    let mut d = Dict::new();
    d.insert(
        "_t".to_string(),
        format!("{} {} {}", translation.x, translation.y, translation.z),
    );
    if let Some(r) = rotation_byte {
        d.insert("_r".to_string(), r.to_string());
    }
    d
}

/// An axis-aligned bounding box in world space, in voxel units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: Vec3,
    pub max: Vec3,
}

impl Bounds {
    pub fn centre(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn extent(&self) -> Vec3 {
        self.max - self.min
    }

    /// Longest diagonal; used to pick a framing distance.
    pub fn diagonal(&self) -> f32 {
        self.extent().length().max(1.0)
    }

    pub fn union(self, other: Bounds) -> Bounds {
        Bounds {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
}

impl Default for Bounds {
    fn default() -> Self {
        Bounds {
            min: Vec3::splat(-0.5),
            max: Vec3::splat(0.5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dot_vox::{Frame, SceneNode, ShapeModel};

    /// Every `_r` byte that actually describes a signed permutation.
    fn valid_bytes() -> Vec<u8> {
        (0u8..=255)
            .filter(|b| Rot3::from_byte(*b).is_some())
            .collect()
    }

    #[test]
    fn magicavoxel_identity_byte_decodes_to_identity() {
        // Row 0 takes column 0, row 1 takes column 1, no sign bits.
        assert_eq!(Rot3::from_byte(0b0000_0100), Some(Rot3::IDENTITY));
    }

    #[test]
    fn non_permutation_bytes_are_rejected() {
        // Rows 0 and 1 claiming the same column, and a column index of 3.
        assert_eq!(Rot3::from_byte(0b0000_0000), None);
        assert_eq!(Rot3::from_byte(0b0000_1111), None);
        // 48 of the low 7 bits are permutations: 6 orderings x 8 signs. Bit 7
        // is unused by the format and ignored, so 96 whole bytes decode.
        assert_eq!(valid_bytes().iter().filter(|b| **b < 128).count(), 48);
        assert_eq!(Rot3::from_byte(0b1000_0100), Some(Rot3::IDENTITY));
    }

    #[test]
    fn rotations_are_signed_permutations() {
        let v = IVec3::new(3, -5, 7);
        for byte in valid_bytes() {
            let r = Rot3::from_byte(byte).unwrap();
            let mut sorted = r.apply(v).to_array().map(i32::abs);
            sorted.sort_unstable();
            assert_eq!(sorted, [3, 5, 7], "byte {byte} did not permute {v:?}");
        }
    }

    #[test]
    fn composing_rotations_matches_applying_them_in_turn() {
        let v = IVec3::new(1, 2, 3);
        for a in valid_bytes() {
            for b in valid_bytes() {
                let (a, b) = (Rot3::from_byte(a).unwrap(), Rot3::from_byte(b).unwrap());
                assert_eq!(a.compose(b).apply(v), a.apply(b.apply(v)));
            }
        }
    }

    #[test]
    fn float_matrix_agrees_with_the_integer_rotation() {
        let v = IVec3::new(2, -3, 5);
        for byte in valid_bytes() {
            let r = Rot3::from_byte(byte).unwrap();
            let expected = r.apply(v).as_vec3();
            assert!((r.to_mat3() * v.as_vec3() - expected).length() < 1e-5);
        }
    }

    #[test]
    fn translation_moves_a_model_by_whole_voxels() {
        // An even-sized model is centred on a cell boundary, so a translation
        // of t puts local voxel v at v + t - size / 2.
        let t = VoxTransform {
            rotation: Rot3::IDENTITY,
            translation: IVec3::new(10, 0, 0),
        };
        let size = IVec3::splat(2);
        assert_eq!(t.voxel_to_world(IVec3::ZERO, size), IVec3::new(9, -1, -1));
        assert_eq!(t.voxel_to_world(IVec3::ONE, size), IVec3::new(10, 0, 0));
    }

    #[test]
    fn odd_sized_models_stay_on_the_integer_grid() {
        // A 3-wide model has a voxel exactly at its centre, which has to land
        // exactly on the translation.
        let t = VoxTransform {
            rotation: Rot3::IDENTITY,
            translation: IVec3::new(4, -2, 7),
        };
        assert_eq!(
            t.voxel_to_world(IVec3::ONE, IVec3::splat(3)),
            IVec3::new(4, -2, 7)
        );
    }

    #[test]
    fn rotation_about_the_centre_preserves_the_voxel_count() {
        // Every rotation has to map the model cells onto a set of the same
        // size. If the halving in `voxel_to_world` ever rounded, cells would
        // collide and the set would shrink.
        let size = IVec3::new(4, 2, 6);
        let cells = (size.x * size.y * size.z) as usize;
        for byte in valid_bytes() {
            let t = VoxTransform {
                rotation: Rot3::from_byte(byte).unwrap(),
                translation: IVec3::new(-3, 11, 2),
            };
            let mut seen = std::collections::HashSet::new();
            for z in 0..size.z {
                for y in 0..size.y {
                    for x in 0..size.x {
                        seen.insert(t.voxel_to_world(IVec3::new(x, y, z), size).to_array());
                    }
                }
            }
            assert_eq!(seen.len(), cells, "byte {byte} collapsed cells");
        }
    }

    #[test]
    fn the_render_matrix_matches_the_integer_placement() {
        // The mesh is built in local 0..size and moved by `model_matrix`; the
        // two have to agree, or the picture disagrees with the bounds.
        let size = IVec3::new(4, 2, 6);
        for byte in valid_bytes() {
            let t = VoxTransform {
                rotation: Rot3::from_byte(byte).unwrap(),
                translation: IVec3::new(5, -1, 3),
            };
            let matrix = t.model_matrix(size);
            for v in [IVec3::ZERO, IVec3::new(3, 1, 5), IVec3::new(1, 0, 2)] {
                // Cell centres: local v + 0.5 has to land on world w + 0.5.
                let got = matrix.transform_point3(v.as_vec3() + Vec3::splat(0.5));
                let want = t.voxel_to_world(v, size).as_vec3() + Vec3::splat(0.5);
                assert!(
                    (got - want).length() < 1e-4,
                    "byte {byte}: {got:?} != {want:?}"
                );
            }
        }
    }

    #[test]
    fn nested_transforms_compose_parent_then_child() {
        // Rotate the child frame a quarter turn about Z, then offset it.
        let parent = VoxTransform {
            // Row 0 takes column 1 negated, row 1 takes column 0: (x, y) -> (-y, x).
            rotation: Rot3::from_byte(0b0001_0001).unwrap(),
            translation: IVec3::new(10, 0, 0),
        };
        let child = VoxTransform {
            rotation: Rot3::IDENTITY,
            translation: IVec3::new(0, 3, -1),
        };
        let composed = parent.compose(child);
        assert_eq!(composed.rotation, parent.rotation);
        assert_eq!(composed.translation, IVec3::new(7, 0, -1));
    }

    #[test]
    fn a_file_with_no_scene_graph_puts_every_model_at_the_origin() {
        let instances = flatten(&[], &[], 3);
        assert_eq!(instances.len(), 3);
        assert!(
            instances
                .iter()
                .all(|i| i.transform == VoxTransform::IDENTITY)
        );
        assert_eq!(instances[2].model_index, 2);
    }

    #[test]
    fn hidden_nodes_and_out_of_range_models_are_skipped() {
        let mut hidden = Dict::new();
        hidden.insert("_hidden".to_string(), "1".to_string());
        let nodes = vec![
            SceneNode::Group {
                attributes: Dict::new(),
                children: vec![1, 3],
            },
            SceneNode::Transform {
                attributes: hidden,
                frames: vec![Frame::new(frame_dict(IVec3::ZERO, None))],
                child: 2,
                layer_id: 0,
            },
            SceneNode::Shape {
                attributes: Dict::new(),
                models: vec![ShapeModel {
                    model_id: 0,
                    attributes: Dict::new(),
                }],
            },
            SceneNode::Shape {
                attributes: Dict::new(),
                // Past the end of the model list: a corrupt file, not a panic.
                models: vec![ShapeModel {
                    model_id: 99,
                    attributes: Dict::new(),
                }],
            },
        ];
        assert!(flatten(&nodes, &[], 1).is_empty());
    }

    #[test]
    fn a_cyclic_scene_graph_terminates() {
        let nodes = vec![
            SceneNode::Group {
                attributes: Dict::new(),
                children: vec![1],
            },
            SceneNode::Group {
                attributes: Dict::new(),
                children: vec![0],
            },
        ];
        assert!(flatten(&nodes, &[], 1).is_empty());
    }

    #[test]
    fn malformed_keyframe_attributes_fall_back_to_identity() {
        let mut d = Dict::new();
        d.insert("_t".to_string(), "not numbers".to_string());
        // 0 is one of the rotation bytes the dot_vox decoder asserts on.
        d.insert("_r".to_string(), "0".to_string());
        assert_eq!(
            frame_transform(Some(&Frame::new(d))),
            VoxTransform::IDENTITY
        );
    }
}
