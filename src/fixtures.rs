//! Synthetic `.vox` files, written from code.
//!
//! Tests and the `--write-fixtures` flag both use these so that nothing in
//! this repository depends on having MagicaVoxel installed. Serialisation goes
//! through `dot_vox`'s own writer, which keeps the fixtures honest: they are
//! real files with real chunk headers, not hand-rolled approximations.

use crate::scene::frame_dict;
use dot_vox::{
    Color, DEFAULT_PALETTE, Dict, DotVoxData, Frame, Layer, Model, SceneNode, ShapeModel, Size,
    Voxel,
};
use glam::IVec3;
use std::path::Path;

/// Names and builders for every fixture, in the order they are written.
pub const FIXTURES: &[(&str, fn() -> DotVoxData)] = &[
    ("cube.vox", single_cube),
    ("checker.vox", checkerboard_8),
    ("two_models.vox", two_models_translated),
];

/// Write every fixture into `dir`, creating it if needed.
pub fn write_all(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (name, build) in FIXTURES {
        let path = dir.join(name);
        let mut bytes = Vec::new();
        build().write_vox(&mut bytes)?;
        std::fs::write(path, bytes)?;
    }
    Ok(())
}

/// Serialise one fixture to bytes.
pub fn to_bytes(data: &DotVoxData) -> Vec<u8> {
    let mut bytes = Vec::new();
    data.write_vox(&mut bytes)
        .expect("writing to a Vec cannot fail");
    bytes
}

/// A solid 4x4x4 cube of one colour, with no scene graph.
pub fn single_cube() -> DotVoxData {
    let mut voxels = Vec::new();
    for z in 0..4u8 {
        for y in 0..4u8 {
            for x in 0..4u8 {
                voxels.push(Voxel { x, y, z, i: 78 });
            }
        }
    }
    base(vec![Model {
        size: Size { x: 4, y: 4, z: 4 },
        voxels,
    }])
}

/// An 8x8x8 checkerboard: the worst case for greedy merging, and an easy way
/// to eyeball whether palette indices are being read correctly.
pub fn checkerboard_8() -> DotVoxData {
    let mut voxels = Vec::new();
    for z in 0..8u8 {
        for y in 0..8u8 {
            for x in 0..8u8 {
                if (x + y + z) % 2 == 0 {
                    // Two alternating palette entries so the pattern is visible.
                    let i = if (x / 2 + y / 2) % 2 == 0 { 215 } else { 79 };
                    voxels.push(Voxel { x, y, z, i });
                }
            }
        }
    }
    base(vec![Model {
        size: Size { x: 8, y: 8, z: 8 },
        voxels,
    }])
}

/// Translation used by [`two_models_translated`] for the first model.
pub const TWO_MODELS_T0: IVec3 = IVec3::new(10, 0, 0);
/// Translation used by [`two_models_translated`] for the second model.
pub const TWO_MODELS_T1: IVec3 = IVec3::new(-6, 2, 1);

/// Two 2x2x2 models placed apart by a scene graph:
/// root `nTRN` -> `nGRP` -> two `nTRN` -> `nSHP`.
pub fn two_models_translated() -> DotVoxData {
    let block = |i: u8| Model {
        size: Size { x: 2, y: 2, z: 2 },
        voxels: (0..8u8)
            .map(|n| Voxel {
                x: n & 1,
                y: (n >> 1) & 1,
                z: (n >> 2) & 1,
                i,
            })
            .collect(),
    };
    let mut data = base(vec![block(78), block(215)]);
    data.scenes = vec![
        SceneNode::Transform {
            attributes: Dict::new(),
            frames: vec![Frame::new(frame_dict(IVec3::ZERO, None))],
            child: 1,
            layer_id: 0,
        },
        SceneNode::Group {
            attributes: Dict::new(),
            children: vec![2, 4],
        },
        SceneNode::Transform {
            attributes: named("left"),
            frames: vec![Frame::new(frame_dict(TWO_MODELS_T0, None))],
            child: 3,
            layer_id: 0,
        },
        SceneNode::Shape {
            attributes: Dict::new(),
            models: vec![ShapeModel {
                model_id: 0,
                attributes: Dict::new(),
            }],
        },
        SceneNode::Transform {
            attributes: named("right"),
            frames: vec![Frame::new(frame_dict(TWO_MODELS_T1, None))],
            child: 5,
            layer_id: 0,
        },
        SceneNode::Shape {
            attributes: Dict::new(),
            models: vec![ShapeModel {
                model_id: 1,
                attributes: Dict::new(),
            }],
        },
    ];
    data.layers = vec![Layer {
        attributes: Dict::new(),
    }];
    data
}

fn named(name: &str) -> Dict {
    let mut d = Dict::new();
    d.insert("_name".to_string(), name.to_string());
    d
}

fn base(models: Vec<Model>) -> DotVoxData {
    let palette: Vec<Color> = DEFAULT_PALETTE.clone();
    DotVoxData {
        version: 150,
        index_map: dot_vox::DEFAULT_INDEX_MAP.to_vec(),
        models,
        palette,
        materials: Vec::new(),
        scenes: Vec::new(),
        layers: Vec::new(),
    }
}
