//! Scene-graph flattening.
//!
//! MagicaVoxel 0.99+ stores a scene graph of `nTRN` (transform), `nGRP`
//! (group) and `nSHP` (shape) chunks. Flattening it gives one *instance* per
//! rendered model, each with an integer rigid transform.
//!
//! Coordinates are MagicaVoxel's own: right-handed, Z up, one unit per voxel.

use glam::{IVec3, Mat3, Mat4, Vec3};
use dot_vox::Dict;

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
    /// MagicaVoxel transforms a model about its centre, and the centre of an
    /// even-sized model sits on a cell boundary. Working in doubled
    /// coordinates (`2 * v + 1 - size` is the doubled voxel centre) keeps the
    /// arithmetic exact: the result is always odd, so halving it back is a
    /// clean integer operation with no rounding to get wrong.
    pub fn voxel_to_world(self, v: IVec3, size: IVec3) -> IVec3 {
        let doubled_local = v * 2 + IVec3::ONE - size;
        let doubled_world = self.rotation.apply(doubled_local) + self.translation * 2;
        (doubled_world - IVec3::ONE) / 2
    }

    /// Matrix that maps a model's *local* mesh (built in `0..size`) into world
    /// space, matching [`VoxTransform::voxel_to_world`] exactly.
    pub fn model_matrix(self, size: IVec3) -> Mat4 {
        let centre = size.as_vec3() * 0.5;
        let rot = self.rotation.to_mat3();
        Mat4::from_translation(self.translation.as_vec3())
            * Mat4::from_mat3(rot)
            * Mat4::from_translation(-centre)
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
