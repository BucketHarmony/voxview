//! Laying out the HUD: a panel of text in the top-left corner.
//!
//! Produces screen-space quads in physical pixels; the shader turns those into
//! clip space. Keeping layout here means it can be unit-tested without a GPU.

use crate::font;
use crate::palette::srgb_to_linear;

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct HudVertex {
    /// Physical pixels, origin top-left.
    pub position: [f32; 2],
    pub uv: [f32; 2],
    /// Linear RGBA.
    pub color: [f32; 4],
}

/// One line of HUD text.
#[derive(Clone, Debug, PartialEq)]
pub struct HudLine {
    pub text: String,
    pub color: Color,
}

/// The small set of colours the HUD uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Normal,
    Dim,
    Error,
}

impl Color {
    fn rgba(self) -> [f32; 4] {
        let srgb = match self {
            Color::Normal => [235u8, 238, 242],
            Color::Dim => [150, 155, 165],
            Color::Error => [255, 120, 100],
        };
        [
            srgb_to_linear(srgb[0] as f32 / 255.0),
            srgb_to_linear(srgb[1] as f32 / 255.0),
            srgb_to_linear(srgb[2] as f32 / 255.0),
            1.0,
        ]
    }
}

impl HudLine {
    pub fn normal(text: impl Into<String>) -> HudLine {
        HudLine {
            text: text.into(),
            color: Color::Normal,
        }
    }

    pub fn dim(text: impl Into<String>) -> HudLine {
        HudLine {
            text: text.into(),
            color: Color::Dim,
        }
    }

    pub fn error(text: impl Into<String>) -> HudLine {
        HudLine {
            text: text.into(),
            color: Color::Error,
        }
    }
}

/// Triangles for the HUD, in the HUD pipeline's vertex format.
#[derive(Clone, Debug, Default)]
pub struct HudMesh {
    pub vertices: Vec<HudVertex>,
    pub indices: Vec<u32>,
}

impl HudMesh {
    fn quad(&mut self, x: f32, y: f32, w: f32, h: f32, uv: [f32; 4], color: [f32; 4]) {
        let base = self.vertices.len() as u32;
        let corners = [
            ([x, y], [uv[0], uv[1]]),
            ([x + w, y], [uv[2], uv[1]]),
            ([x + w, y + h], [uv[2], uv[3]]),
            ([x, y + h], [uv[0], uv[3]]),
        ];
        for (position, uv) in corners {
            self.vertices.push(HudVertex {
                position,
                uv,
                color,
            });
        }
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

/// Margin between the window edge and the panel, in unscaled pixels.
const MARGIN: f32 = 8.0;
/// Padding inside the panel.
const PADDING: f32 = 6.0;
/// Extra space between baselines, on top of the glyph cell height.
const LEADING: f32 = 2.0;

/// Lay out `lines` as a panel anchored to the top-left corner.
///
/// `scale` is an integer pixel scale, so glyphs stay crisp on HiDPI displays.
pub fn layout(lines: &[HudLine], scale: f32) -> HudMesh {
    let mut mesh = HudMesh::default();
    if lines.is_empty() {
        return mesh;
    }
    let cell_w = font::CELL_W as f32 * scale;
    let line_h = (font::CELL_H as f32 + LEADING) * scale;
    let margin = MARGIN * scale;
    let padding = PADDING * scale;

    let widest = lines
        .iter()
        .map(|l| l.text.chars().count())
        .max()
        .unwrap_or(0) as f32;
    let panel_w = widest * cell_w + padding * 2.0;
    let panel_h = lines.len() as f32 * line_h + padding * 2.0;

    // A dark translucent backing so text stays readable over either
    // background colour and over the model itself.
    mesh.quad(
        margin,
        margin,
        panel_w,
        panel_h,
        font::cell_uv(font::SOLID_CELL),
        [0.0, 0.0, 0.0, 0.55],
    );

    for (row, line) in lines.iter().enumerate() {
        let color = line.color.rgba();
        let y = margin + padding + row as f32 * line_h;
        for (col, ch) in line.text.chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            mesh.quad(
                margin + padding + col as f32 * cell_w,
                y,
                cell_w,
                font::CELL_H as f32 * scale,
                font::cell_uv(font::glyph_cell(ch)),
                color,
            );
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_makes_no_geometry() {
        assert!(layout(&[], 2.0).is_empty());
    }

    #[test]
    fn one_quad_per_visible_glyph_plus_the_panel() {
        let lines = vec![HudLine::normal("ab c"), HudLine::dim("d")];
        let mesh = layout(&lines, 2.0);
        // 4 non-space glyphs + 1 panel = 5 quads.
        assert_eq!(mesh.vertices.len(), 5 * 4);
        assert_eq!(mesh.indices.len(), 5 * 6);
    }

    #[test]
    fn panel_grows_to_the_widest_line() {
        let narrow = layout(&[HudLine::normal("ab")], 1.0);
        let wide = layout(&[HudLine::normal("abcdefgh")], 1.0);
        let width = |m: &HudMesh| m.vertices[1].position[0] - m.vertices[0].position[0];
        assert!(width(&wide) > width(&narrow));
    }

    #[test]
    fn everything_stays_inside_the_top_left_quadrant() {
        let mesh = layout(
            &[HudLine::normal("voxview"), HudLine::error("bad file")],
            2.0,
        );
        for v in &mesh.vertices {
            assert!(v.position[0] >= 0.0 && v.position[1] >= 0.0);
        }
    }

    #[test]
    fn indices_stay_in_range() {
        let mesh = layout(&[HudLine::normal("0123456789")], 3.0);
        for &i in &mesh.indices {
            assert!((i as usize) < mesh.vertices.len());
        }
    }
}
