//! Palette handling.
//!
//! MagicaVoxel stores an optional 256-entry `RGBA` chunk. Voxel indices in
//! `XYZI` are 1-based in the file; `dot_vox` has already subtracted one by the
//! time we see them, so `Palette::color(voxel.i)` is a direct lookup.

/// Convert one sRGB-encoded channel in `0..=1` to linear light.
///
/// Surfaces are created with an sRGB format, so the GPU encodes whatever a
/// shader writes. Colours therefore have to reach the shader already linear,
/// or everything comes out washed out.
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// A single palette entry, stored as linear-order RGBA bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    /// sRGB-encoded bytes as the renderer wants them (`r, g, b, a`).
    pub fn to_array(self) -> [u8; 4] {
        self.0
    }
}

/// A full 256-entry palette. Always exactly 256 entries so indexing a voxel
/// colour can never be out of bounds, however corrupt the source file is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    entries: [Rgba; 256],
    /// True when the source file actually carried an `RGBA` chunk.
    pub from_file: bool,
}

impl Default for Palette {
    fn default() -> Self {
        Self::magicavoxel_default()
    }
}

impl Palette {
    /// MagicaVoxel's built-in palette, used when a file has no `RGBA` chunk.
    pub fn magicavoxel_default() -> Self {
        let mut entries = [Rgba([0, 0, 0, 255]); 256];
        for (dst, src) in entries.iter_mut().zip(DEFAULT_PALETTE_ABGR.iter()) {
            let v = *src;
            *dst = Rgba([v as u8, (v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8]);
        }
        Palette {
            entries,
            from_file: false,
        }
    }

    /// Build from the colours `dot_vox` parsed out of an `RGBA` chunk.
    ///
    /// Short palettes are padded from the default palette and long ones are
    /// truncated, so a malformed chunk degrades instead of failing.
    pub fn from_file_colors(colors: &[dot_vox::Color]) -> Self {
        let mut pal = Self::magicavoxel_default();
        for (dst, src) in pal.entries.iter_mut().zip(colors.iter()) {
            *dst = Rgba([src.r, src.g, src.b, src.a]);
        }
        pal.from_file = !colors.is_empty();
        pal
    }

    /// Colour for a 0-based voxel palette index.
    pub fn color(&self, index: u8) -> Rgba {
        self.entries[index as usize]
    }

    /// All 256 entries as linear `vec4`s, ready to be a GPU uniform.
    pub fn to_linear_rgba(&self) -> [[f32; 4]; 256] {
        let mut out = [[0.0f32; 4]; 256];
        for (dst, src) in out.iter_mut().zip(self.entries.iter()) {
            *dst = [
                srgb_to_linear(src.0[0] as f32 / 255.0),
                srgb_to_linear(src.0[1] as f32 / 255.0),
                srgb_to_linear(src.0[2] as f32 / 255.0),
                src.0[3] as f32 / 255.0,
            ];
        }
        out
    }

    /// Flat `r, g, b, a` bytes for all 256 entries, for upload to the GPU.
    pub fn to_rgba_bytes(&self) -> [u8; 1024] {
        let mut out = [0u8; 1024];
        for (i, e) in self.entries.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&e.0);
        }
        out
    }

    // EXTENSION: a palette remap tool would live here -- take a mapping of
    // `old_index -> new_index` (or a whole replacement palette loaded from
    // another `.vox`), permute `entries`, and re-upload the palette buffer.
    // Because face colours are palette *indices* on the GPU rather than baked
    // vertex colours, a remap needs no re-meshing at all.
}

/// MagicaVoxel's default palette, in the file's own `0xAABBGGRR` word order.
#[rustfmt::skip]
const DEFAULT_PALETTE_ABGR: [u32; 256] = [
    0xffffffff, 0xffccffff, 0xff99ffff, 0xff66ffff, 0xff33ffff, 0xff00ffff,
    0xffffccff, 0xffccccff, 0xff99ccff, 0xff66ccff, 0xff33ccff, 0xff00ccff,
    0xffff99ff, 0xffcc99ff, 0xff9999ff, 0xff6699ff, 0xff3399ff, 0xff0099ff,
    0xffff66ff, 0xffcc66ff, 0xff9966ff, 0xff6666ff, 0xff3366ff, 0xff0066ff,
    0xffff33ff, 0xffcc33ff, 0xff9933ff, 0xff6633ff, 0xff3333ff, 0xff0033ff,
    0xffff00ff, 0xffcc00ff, 0xff9900ff, 0xff6600ff, 0xff3300ff, 0xff0000ff,
    0xffffffcc, 0xffccffcc, 0xff99ffcc, 0xff66ffcc, 0xff33ffcc, 0xff00ffcc,
    0xffffcccc, 0xffcccccc, 0xff99cccc, 0xff66cccc, 0xff33cccc, 0xff00cccc,
    0xffff99cc, 0xffcc99cc, 0xff9999cc, 0xff6699cc, 0xff3399cc, 0xff0099cc,
    0xffff66cc, 0xffcc66cc, 0xff9966cc, 0xff6666cc, 0xff3366cc, 0xff0066cc,
    0xffff33cc, 0xffcc33cc, 0xff9933cc, 0xff6633cc, 0xff3333cc, 0xff0033cc,
    0xffff00cc, 0xffcc00cc, 0xff9900cc, 0xff6600cc, 0xff3300cc, 0xff0000cc,
    0xffffff99, 0xffccff99, 0xff99ff99, 0xff66ff99, 0xff33ff99, 0xff00ff99,
    0xffffcc99, 0xffcccc99, 0xff99cc99, 0xff66cc99, 0xff33cc99, 0xff00cc99,
    0xffff9999, 0xffcc9999, 0xff999999, 0xff669999, 0xff339999, 0xff009999,
    0xffff6699, 0xffcc6699, 0xff996699, 0xff666699, 0xff336699, 0xff006699,
    0xffff3399, 0xffcc3399, 0xff993399, 0xff663399, 0xff333399, 0xff003399,
    0xffff0099, 0xffcc0099, 0xff990099, 0xff660099, 0xff330099, 0xff000099,
    0xffffff66, 0xffccff66, 0xff99ff66, 0xff66ff66, 0xff33ff66, 0xff00ff66,
    0xffffcc66, 0xffcccc66, 0xff99cc66, 0xff66cc66, 0xff33cc66, 0xff00cc66,
    0xffff9966, 0xffcc9966, 0xff999966, 0xff669966, 0xff339966, 0xff009966,
    0xffff6666, 0xffcc6666, 0xff996666, 0xff666666, 0xff336666, 0xff006666,
    0xffff3366, 0xffcc3366, 0xff993366, 0xff663366, 0xff333366, 0xff003366,
    0xffff0066, 0xffcc0066, 0xff990066, 0xff660066, 0xff330066, 0xff000066,
    0xffffff33, 0xffccff33, 0xff99ff33, 0xff66ff33, 0xff33ff33, 0xff00ff33,
    0xffffcc33, 0xffcccc33, 0xff99cc33, 0xff66cc33, 0xff33cc33, 0xff00cc33,
    0xffff9933, 0xffcc9933, 0xff999933, 0xff669933, 0xff339933, 0xff009933,
    0xffff6633, 0xffcc6633, 0xff996633, 0xff666633, 0xff336633, 0xff006633,
    0xffff3333, 0xffcc3333, 0xff993333, 0xff663333, 0xff333333, 0xff003333,
    0xffff0033, 0xffcc0033, 0xff990033, 0xff660033, 0xff330033, 0xff000033,
    0xffffff00, 0xffccff00, 0xff99ff00, 0xff66ff00, 0xff33ff00, 0xff00ff00,
    0xffffcc00, 0xffcccc00, 0xff99cc00, 0xff66cc00, 0xff33cc00, 0xff00cc00,
    0xffff9900, 0xffcc9900, 0xff999900, 0xff669900, 0xff339900, 0xff009900,
    0xffff6600, 0xffcc6600, 0xff996600, 0xff666600, 0xff336600, 0xff006600,
    0xffff3300, 0xffcc3300, 0xff993300, 0xff663300, 0xff333300, 0xff003300,
    0xffff0000, 0xffcc0000, 0xff990000, 0xff660000, 0xff330000, 0xff0000ee,
    0xff0000dd, 0xff0000bb, 0xff0000aa, 0xff000088, 0xff000077, 0xff000055,
    0xff000044, 0xff000022, 0xff000011, 0xff00ee00, 0xff00dd00, 0xff00bb00,
    0xff00aa00, 0xff008800, 0xff007700, 0xff005500, 0xff004400, 0xff002200,
    0xff001100, 0xffee0000, 0xffdd0000, 0xffbb0000, 0xffaa0000, 0xff880000,
    0xff770000, 0xff550000, 0xff440000, 0xff220000, 0xff110000, 0xffeeeeee,
    0xffdddddd, 0xffbbbbbb, 0xffaaaaaa, 0xff888888, 0xff777777, 0xff555555,
    0xff444444, 0xff222222, 0xff111111, 0x00000000,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_palette_has_expected_corners() {
        let p = Palette::magicavoxel_default();
        // First entry of MagicaVoxel's default palette is opaque white.
        assert_eq!(p.color(0).to_array(), [255, 255, 255, 255]);
        assert!(!p.from_file);
    }

    #[test]
    fn short_file_palette_is_padded_not_truncated() {
        let colors = vec![dot_vox::Color {
            r: 1,
            g: 2,
            b: 3,
            a: 4,
        }];
        let p = Palette::from_file_colors(&colors);
        assert_eq!(p.color(0).to_array(), [1, 2, 3, 4]);
        // Everything past the supplied entry falls back to the default palette.
        assert_eq!(
            p.color(255).to_array(),
            Palette::magicavoxel_default().color(255).to_array()
        );
        assert!(p.from_file);
    }

    #[test]
    fn srgb_conversion_hits_the_endpoints() {
        assert!((srgb_to_linear(0.0)).abs() < 1e-6);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-6);
        // Mid grey is much darker in linear light than its sRGB code suggests.
        assert!(srgb_to_linear(0.5) < 0.25);
    }

    #[test]
    fn rgba_bytes_round_trip() {
        let p = Palette::magicavoxel_default();
        let bytes = p.to_rgba_bytes();
        for i in 0..256usize {
            assert_eq!(&bytes[i * 4..i * 4 + 4], &p.color(i as u8).to_array());
        }
    }
}
