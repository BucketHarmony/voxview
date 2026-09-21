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
    /// Normal text, drawn on a highlight bar spanning the panel. The menu uses
    /// it for the row under the cursor.
    Selected,
}

impl Color {
    /// The bar drawn behind a [`Color::Selected`] row, if any.
    fn backing(self) -> Option<[f32; 4]> {
        match self {
            Color::Selected => Some([0.36, 0.52, 0.78, 0.85]),
            _ => None,
        }
    }

    fn rgba(self) -> [f32; 4] {
        let srgb = match self {
            Color::Normal | Color::Selected => [235u8, 238, 242],
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

    pub fn selected(text: impl Into<String>) -> HudLine {
        HudLine {
            text: text.into(),
            color: Color::Selected,
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

    /// Append another mesh, shifting its indices to match.
    pub fn append(&mut self, other: &HudMesh) {
        let base = self.vertices.len() as u32;
        self.vertices.extend_from_slice(&other.vertices);
        self.indices.extend(other.indices.iter().map(|i| i + base));
    }
}

/// Which corner a panel hangs from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// The stats panel.
    TopLeft,
    /// The file menu, so the two never overlap.
    TopRight,
}

/// Panel geometry in physical pixels at a given integer scale.
///
/// Both [`layout`] and the menu's hit-testing derive their coordinates from
/// this, so a click always lands on the row that was actually drawn. Keeping
/// the arithmetic in one place is the whole point: two copies drift the moment
/// the padding changes.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub cell_w: f32,
    pub glyph_h: f32,
    pub line_h: f32,
    pub margin: f32,
    pub padding: f32,
}

/// Metrics for an integer pixel `scale`.
pub fn metrics(scale: f32) -> Metrics {
    Metrics {
        cell_w: font::CELL_W as f32 * scale,
        glyph_h: font::CELL_H as f32 * scale,
        line_h: (font::CELL_H as f32 + LEADING) * scale,
        margin: MARGIN * scale,
        padding: PADDING * scale,
    }
}

impl Metrics {
    /// Size of a panel holding `lines`, in pixels.
    pub fn panel_size(&self, lines: &[HudLine]) -> (f32, f32) {
        let widest = lines
            .iter()
            .map(|l| l.text.chars().count())
            .max()
            .unwrap_or(0) as f32;
        (
            widest * self.cell_w + self.padding * 2.0,
            lines.len() as f32 * self.line_h + self.padding * 2.0,
        )
    }

    /// Top-left corner of a panel of `size` in a `viewport`-sized window.
    pub fn origin(&self, anchor: Anchor, size: (f32, f32), viewport: (f32, f32)) -> (f32, f32) {
        let x = match anchor {
            Anchor::TopLeft => self.margin,
            // Clamped so an over-wide panel spills off the right rather than
            // off the left, where it would cover the stats.
            Anchor::TopRight => (viewport.0 - self.margin - size.0).max(self.margin),
        };
        (x, self.margin)
    }

    /// Top edge of row `row` in a panel whose corner is at `origin`.
    pub fn row_top(&self, origin: (f32, f32), row: usize) -> f32 {
        origin.1 + self.padding + row as f32 * self.line_h
    }

    /// How many rows fit in a window `viewport_h` pixels tall.
    pub fn rows_that_fit(&self, viewport_h: f32) -> usize {
        let usable = viewport_h - (self.margin + self.padding) * 2.0;
        (usable / self.line_h).floor().max(1.0) as usize
    }
}

/// Margin between the window edge and the panel, in unscaled pixels.
const MARGIN: f32 = 8.0;
/// Padding inside the panel.
const PADDING: f32 = 6.0;
/// Extra space between baselines, on top of the glyph cell height.
const LEADING: f32 = 2.0;

/// Lay out `lines` as a panel in one corner of a `viewport`-sized window.
///
/// `scale` is an integer pixel scale, so glyphs stay crisp on HiDPI displays.
pub fn layout(lines: &[HudLine], scale: f32, anchor: Anchor, viewport: (f32, f32)) -> HudMesh {
    let mut mesh = HudMesh::default();
    if lines.is_empty() {
        return mesh;
    }
    let m = metrics(scale);
    let size = m.panel_size(lines);
    let origin = m.origin(anchor, size, viewport);
    let solid = font::cell_uv(font::SOLID_CELL);

    // A dark translucent backing so text stays readable over either
    // background colour and over the model itself.
    mesh.quad(
        origin.0,
        origin.1,
        size.0,
        size.1,
        solid,
        [0.0, 0.0, 0.0, 0.55],
    );

    for (row, line) in lines.iter().enumerate() {
        let color = line.color.rgba();
        let y = m.row_top(origin, row);
        // The bar spans the panel so a selected row reads as one block, not as
        // a ragged strip the width of its own text.
        if let Some(bar) = line.color.backing() {
            mesh.quad(
                origin.0 + m.padding * 0.5,
                y,
                size.0 - m.padding,
                m.line_h,
                solid,
                bar,
            );
        }
        for (col, ch) in line.text.chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            mesh.quad(
                origin.0 + m.padding + col as f32 * m.cell_w,
                y,
                m.cell_w,
                m.glyph_h,
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

    const VIEW: (f32, f32) = (1280.0, 800.0);

    fn left(lines: &[HudLine], scale: f32) -> HudMesh {
        layout(lines, scale, Anchor::TopLeft, VIEW)
    }

    #[test]
    fn empty_input_makes_no_geometry() {
        assert!(left(&[], 2.0).is_empty());
    }

    #[test]
    fn one_quad_per_visible_glyph_plus_the_panel() {
        let lines = vec![HudLine::normal("ab c"), HudLine::dim("d")];
        let mesh = left(&lines, 2.0);
        // 4 non-space glyphs + 1 panel = 5 quads.
        assert_eq!(mesh.vertices.len(), 5 * 4);
        assert_eq!(mesh.indices.len(), 5 * 6);
    }

    #[test]
    fn a_selected_row_adds_exactly_one_bar() {
        let plain = left(&[HudLine::normal("ab")], 2.0);
        let picked = left(&[HudLine::selected("ab")], 2.0);
        assert_eq!(picked.vertices.len(), plain.vertices.len() + 4);
    }

    #[test]
    fn panel_grows_to_the_widest_line() {
        let narrow = left(&[HudLine::normal("ab")], 1.0);
        let wide = left(&[HudLine::normal("abcdefgh")], 1.0);
        let width = |m: &HudMesh| m.vertices[1].position[0] - m.vertices[0].position[0];
        assert!(width(&wide) > width(&narrow));
    }

    #[test]
    fn everything_stays_inside_the_top_left_quadrant() {
        let mesh = left(
            &[HudLine::normal("voxview"), HudLine::error("bad file")],
            2.0,
        );
        for v in &mesh.vertices {
            assert!(v.position[0] >= 0.0 && v.position[1] >= 0.0);
        }
    }

    #[test]
    fn a_right_anchored_panel_ends_at_the_right_margin() {
        let lines = [HudLine::normal("menu")];
        let mesh = layout(&lines, 2.0, Anchor::TopRight, VIEW);
        let m = metrics(2.0);
        // Vertex 1 is the panel quad's top-right corner.
        assert!((mesh.vertices[1].position[0] - (VIEW.0 - m.margin)).abs() < 1e-3);
        // And it must not reach back over the stats panel on the left.
        assert!(mesh.vertices[0].position[0] > VIEW.0 / 2.0);
    }

    #[test]
    fn an_over_wide_panel_keeps_its_left_edge_on_screen() {
        let wide = "x".repeat(500);
        let mesh = layout(&[HudLine::normal(wide)], 2.0, Anchor::TopRight, VIEW);
        assert!(mesh.vertices[0].position[0] >= metrics(2.0).margin);
    }

    #[test]
    fn row_tops_follow_the_drawn_glyphs() {
        // Hit-testing trusts `row_top`, so it has to agree with `layout`.
        let lines = [
            HudLine::normal("a"),
            HudLine::normal("b"),
            HudLine::normal("c"),
        ];
        let mesh = left(&lines, 2.0);
        let m = metrics(2.0);
        let origin = m.origin(Anchor::TopLeft, m.panel_size(&lines), VIEW);
        for row in 0..lines.len() {
            // Quad 0 is the panel; row `row` is the quad after it.
            let glyph_top = mesh.vertices[(row + 1) * 4].position[1];
            assert!((glyph_top - m.row_top(origin, row)).abs() < 1e-3);
        }
    }

    #[test]
    fn rows_that_fit_leaves_room_for_the_margins() {
        let m = metrics(2.0);
        let rows = m.rows_that_fit(800.0);
        assert!(rows >= 1);
        let height = rows as f32 * m.line_h + (m.margin + m.padding) * 2.0;
        assert!(height <= 800.0, "{rows} rows overflow an 800px window");
        // A window too short for even one row still reports one, rather than
        // zero, so the menu never renders as an empty box.
        assert_eq!(m.rows_that_fit(1.0), 1);
    }

    #[test]
    fn appending_shifts_indices_onto_the_appended_vertices() {
        let mut a = left(&[HudLine::normal("ab")], 1.0);
        let b = layout(&[HudLine::normal("cd")], 1.0, Anchor::TopRight, VIEW);
        let (av, bv) = (a.vertices.len(), b.vertices.len());
        a.append(&b);
        assert_eq!(a.vertices.len(), av + bv);
        for &i in &a.indices {
            assert!((i as usize) < a.vertices.len());
        }
        // The tail must address the appended block, not the original.
        assert!(a.indices[a.indices.len() - 1] as usize >= av);
    }

    #[test]
    fn indices_stay_in_range() {
        let mesh = left(&[HudLine::normal("0123456789")], 3.0);
        for &i in &mesh.indices {
            assert!((i as usize) < mesh.vertices.len());
        }
    }
}
