//! Line geometry for the ground grid, bounding box and axis gizmo.
//!
//! All three live in one vertex buffer; each toggle just draws its own range.

use crate::palette::srgb_to_linear;
use crate::scene::Bounds;
use glam::Vec3;
use std::ops::Range;

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LineVertex {
    pub position: [f32; 3],
    /// Linear RGBA, because the swapchain is sRGB and encodes on write.
    pub color: [f32; 4],
}

/// One buffer's worth of overlay lines, plus the ranges to draw for each.
#[derive(Clone, Debug, Default)]
pub struct Overlays {
    pub vertices: Vec<LineVertex>,
    pub grid: Range<u32>,
    pub bbox: Range<u32>,
    pub axes: Range<u32>,
}

const GRID_MINOR: [u8; 3] = [72, 74, 82];
const GRID_MAJOR: [u8; 3] = [112, 115, 126];
const BBOX: [u8; 3] = [240, 190, 70];
const AXIS_X: [u8; 3] = [230, 70, 70];
const AXIS_Y: [u8; 3] = [90, 210, 90];
const AXIS_Z: [u8; 3] = [90, 140, 240];

fn rgba(srgb: [u8; 3], alpha: f32) -> [f32; 4] {
    [
        srgb_to_linear(srgb[0] as f32 / 255.0),
        srgb_to_linear(srgb[1] as f32 / 255.0),
        srgb_to_linear(srgb[2] as f32 / 255.0),
        alpha,
    ]
}

/// Build every overlay for a scene of the given world bounds.
pub fn build(bounds: &Bounds) -> Overlays {
    let mut out = Overlays::default();
    let start = 0;
    grid(&mut out.vertices, bounds);
    out.grid = start..out.vertices.len() as u32;

    let start = out.vertices.len() as u32;
    bbox(&mut out.vertices, bounds);
    out.bbox = start..out.vertices.len() as u32;

    let start = out.vertices.len() as u32;
    axes(&mut out.vertices, bounds);
    out.axes = start..out.vertices.len() as u32;
    out
}

fn segment(out: &mut Vec<LineVertex>, a: Vec3, b: Vec3, color: [f32; 4]) {
    out.push(LineVertex {
        position: a.to_array(),
        color,
    });
    out.push(LineVertex {
        position: b.to_array(),
        color,
    });
}

/// A grid on the z = 0 plane, spaced so the line count stays readable
/// whatever the model's size.
fn grid(out: &mut Vec<LineVertex>, bounds: &Bounds) {
    let extent = bounds.extent().truncate().max_element().max(8.0);
    let half_target = extent * 0.75;
    // Aim for about 32 cells from the centre to the edge whatever the model's
    // size: the line count stays bounded and the spacing stays a round number
    // of voxels, so the grid can still be counted against.
    let step = (half_target / 32.0).max(1.0).log2().ceil().exp2();
    let half = ((half_target / step).ceil() * step).max(step * 4.0);
    // Anchor on the model's footprint, snapped to the grid spacing, so the
    // lines stay put while the camera moves.
    let centre = bounds.centre();
    let cx = (centre.x / step).round() * step;
    let cy = (centre.y / step).round() * step;

    let count = (half / step) as i32;
    for i in -count..=count {
        let offset = i as f32 * step;
        // Every eighth line, and the two through the centre, are brighter.
        let major = i == 0 || i % 8 == 0;
        let color = rgba(if major { GRID_MAJOR } else { GRID_MINOR }, 1.0);
        segment(
            out,
            Vec3::new(cx + offset, cy - half, 0.0),
            Vec3::new(cx + offset, cy + half, 0.0),
            color,
        );
        segment(
            out,
            Vec3::new(cx - half, cy + offset, 0.0),
            Vec3::new(cx + half, cy + offset, 0.0),
            color,
        );
    }
}

/// The twelve edges of the scene's bounding box.
fn bbox(out: &mut Vec<LineVertex>, bounds: &Bounds) {
    let color = rgba(BBOX, 1.0);
    let (lo, hi) = (bounds.min, bounds.max);
    let corner = |i: usize| {
        Vec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        )
    };
    // Each edge joins two corners differing in exactly one bit.
    for i in 0..8usize {
        for bit in [1usize, 2, 4] {
            if i & bit == 0 {
                segment(out, corner(i), corner(i | bit), color);
            }
        }
    }
}

/// Unit axes at the world origin, scaled to the model.
fn axes(out: &mut Vec<LineVertex>, bounds: &Bounds) {
    let length = (bounds.extent().max_element() * 0.6).max(4.0);
    segment(out, Vec3::ZERO, Vec3::X * length, rgba(AXIS_X, 1.0));
    segment(out, Vec3::ZERO, Vec3::Y * length, rgba(AXIS_Y, 1.0));
    segment(out, Vec3::ZERO, Vec3::Z * length, rgba(AXIS_Z, 1.0));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(half: f32) -> Bounds {
        Bounds {
            min: Vec3::splat(-half),
            max: Vec3::splat(half),
        }
    }

    #[test]
    fn ranges_tile_the_buffer_without_gaps() {
        let o = build(&bounds(16.0));
        assert_eq!(o.grid.start, 0);
        assert_eq!(o.grid.end, o.bbox.start);
        assert_eq!(o.bbox.end, o.axes.start);
        assert_eq!(o.axes.end as usize, o.vertices.len());
    }

    #[test]
    fn box_has_twelve_edges_and_gizmo_three() {
        let o = build(&bounds(16.0));
        assert_eq!(o.bbox.len(), 24);
        assert_eq!(o.axes.len(), 6);
    }

    #[test]
    fn grid_line_count_stays_bounded_as_models_grow() {
        for half in [4.0f32, 32.0, 128.0, 1024.0] {
            let o = build(&bounds(half));
            assert!(
                o.grid.len() <= 400,
                "grid for half-extent {half} used {} vertices",
                o.grid.len()
            );
            assert!(o.grid.len() >= 4);
        }
    }

    #[test]
    fn grid_lies_on_the_ground_plane() {
        let o = build(&bounds(16.0));
        for v in &o.vertices[o.grid.start as usize..o.grid.end as usize] {
            assert_eq!(v.position[2], 0.0);
        }
    }
}
