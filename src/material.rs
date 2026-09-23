//! MagicaVoxel's `MATL` chunks, turned into something a rasteriser can use.
//!
//! A `.vox` file carries up to 256 materials, one per palette entry, and a
//! voxel's material is decided entirely by its colour. That is a gift for a
//! renderer that already keeps colour as a palette index: the material table
//! is a second 256-entry uniform indexed by the same number, so a model with
//! glowing windows still meshes and draws exactly like one without.
//!
//! MagicaVoxel is a path tracer and this is not, so most of what a material
//! records cannot be honoured. What survives the translation is the part that
//! changes the silhouette of a model in a preview: what glows, what is shiny,
//! and what you can see through.

/// One palette entry's surface properties.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    /// Emission strength. 0 for a surface that only reflects light; 1 is
    /// roughly "as bright as full daylight on white", and it climbs from
    /// there.
    pub emit: f32,
    /// 0 for a dielectric, 1 for bare metal.
    pub metal: f32,
    /// 0 is a mirror-sharp highlight, 1 is no highlight at all.
    pub rough: f32,
    /// Coverage: 1 is opaque, 0 is invisible.
    pub opacity: f32,
}

impl Default for Material {
    fn default() -> Material {
        Material {
            emit: 0.0,
            metal: 0.0,
            // Diffuse surfaces get no highlight at all, which is both what
            // MagicaVoxel's own preview shows and what keeps a file with no
            // materials looking exactly as it did before they were read.
            rough: 1.0,
            opacity: 1.0,
        }
    }
}

impl Material {
    pub fn is_emissive(&self) -> bool {
        self.emit > 0.0
    }

    pub fn is_metal(&self) -> bool {
        self.metal > 0.0
    }

    /// True when the surface needs the blended pass rather than the opaque
    /// one. The threshold is a pixel's worth of alpha, below which the
    /// difference cannot be seen but the cost of sorting is still paid.
    pub fn is_transparent(&self) -> bool {
        self.opacity < 1.0 - 1.0 / 255.0
    }

    /// As the shader wants it.
    fn to_array(self) -> [f32; 4] {
        [self.emit, self.metal, self.rough, self.opacity]
    }
}

/// A full 256-entry material table, one per palette index.
#[derive(Clone, Debug, PartialEq)]
pub struct Materials {
    entries: [Material; 256],
    /// True when the file actually carried `MATL` chunks worth honouring.
    pub from_file: bool,
}

impl Default for Materials {
    fn default() -> Materials {
        Materials {
            entries: [Material::default(); 256],
            from_file: false,
        }
    }
}

impl Materials {
    /// Read the table out of `dot_vox`'s parsed materials.
    pub fn from_dot_vox(materials: &[dot_vox::Material]) -> Materials {
        let mut table = Materials::default();
        let shift = if zero_based(materials) { 0 } else { 1 };
        for m in materials {
            let Some(index) = m.id.checked_sub(shift) else {
                continue;
            };
            let Some(slot) = table.entries.get_mut(index as usize) else {
                continue;
            };
            let read = read_material(m);
            *slot = read;
            table.from_file |= read != Material::default();
        }
        table
    }

    pub fn get(&self, index: u8) -> Material {
        self.entries[index as usize]
    }

    /// All 256 entries, ready to be a GPU uniform.
    pub fn to_gpu(&self) -> [[f32; 4]; 256] {
        let mut out = [[0.0f32; 4]; 256];
        for (dst, src) in out.iter_mut().zip(self.entries.iter()) {
            *dst = src.to_array();
        }
        out
    }

    /// A 256-bit mask of the entries that need the blended pass, as four
    /// words. The mesher takes this rather than the whole table, because
    /// which pass a face belongs in is all it needs to know.
    pub fn transparent_mask(&self) -> [u64; 4] {
        let mut mask = [0u64; 4];
        for (i, m) in self.entries.iter().enumerate() {
            if m.is_transparent() {
                mask[i / 64] |= 1 << (i % 64);
            }
        }
        mask
    }

    pub fn emissive_count(&self) -> usize {
        self.entries.iter().filter(|m| m.is_emissive()).count()
    }

    pub fn metal_count(&self) -> usize {
        self.entries.iter().filter(|m| m.is_metal()).count()
    }

    pub fn transparent_count(&self) -> usize {
        self.entries.iter().filter(|m| m.is_transparent()).count()
    }
}

/// Which end of the range the file numbers materials from.
///
/// MagicaVoxel numbers them from 1, matching the palette indices as they
/// appear in the file -- so a full table runs 1 to 256, and `dot_vox`, which
/// subtracts one from *voxel* indices but leaves material ids alone, hands
/// them over one too high. Not every writer agrees, so the file is asked:
/// an id of 0 can only come from one that numbered from zero, and an id of
/// 256 only from one that numbered from one. A file that says neither gets
/// MagicaVoxel's own convention.
fn zero_based(materials: &[dot_vox::Material]) -> bool {
    materials.iter().any(|m| m.id == 0) && !materials.iter().any(|m| m.id >= 256)
}

/// Read one material, honouring only the properties its `_type` actually uses.
///
/// MagicaVoxel leaves the values of inactive sliders in the dictionary, so a
/// `_emit` material in a real file will happily carry a `_metal` of 0.77 that
/// its own renderer ignores. Taking every key at face value makes emissive
/// surfaces shiny and glass metallic.
fn read_material(m: &dot_vox::Material) -> Material {
    let kind = m.material_type().unwrap_or("_diffuse");
    let unit = |v: Option<f32>, fallback: f32| match v {
        Some(v) if v.is_finite() => v.clamp(0.0, 1.0),
        _ => fallback,
    };

    let emit = if kind == "_emit" {
        // `_flux` is a stop of radiant power on top of `_emit`, so a lamp at
        // flux 2 is three times the lamp at flux 0. Clamped because the file
        // format does not promise a range and a NaN here would take the whole
        // frame with it.
        let flux = m.radiant_flux().filter(|f| f.is_finite()).unwrap_or(0.0);
        unit(m.emission(), 0.0) * (flux.clamp(0.0, 4.0) + 1.0)
    } else {
        0.0
    };

    let metal = if kind == "_metal" {
        unit(m.metalness(), 0.0)
    } else {
        0.0
    };

    let rough = match kind {
        "_metal" | "_glass" | "_emit" => unit(m.roughness(), 0.5),
        _ => 1.0,
    };

    // `_alpha` reads as "how much light passes through", the opposite way
    // round from a compositing alpha, so it is subtracted rather than used.
    // Older files spell the same idea `_trans`.
    let opacity = match kind {
        "_glass" | "_media" => 1.0 - unit(m.opacity().or_else(|| m.transparency()), 0.0),
        _ => 1.0,
    };

    Material {
        emit,
        metal,
        rough,
        opacity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dot_vox::Material as DotMaterial;

    fn material(id: u32, props: &[(&str, &str)]) -> DotMaterial {
        DotMaterial {
            id,
            properties: props
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn a_file_with_no_materials_leaves_every_surface_plain() {
        let table = Materials::from_dot_vox(&[]);
        assert!(!table.from_file);
        assert_eq!(table.get(0), Material::default());
        assert_eq!(table.get(255), Material::default());
        assert_eq!(table.transparent_count(), 0);
    }

    #[test]
    fn magicavoxel_ids_are_one_ahead_of_the_palette() {
        // Id 1 is the colour a voxel with index 0 is painted.
        let table = Materials::from_dot_vox(&[
            material(1, &[("_type", "_emit"), ("_emit", "1")]),
            material(256, &[("_type", "_emit"), ("_emit", "1")]),
        ]);
        assert!(table.get(0).is_emissive(), "id 1 should light up index 0");
        assert!(
            table.get(255).is_emissive(),
            "id 256 should light up index 255"
        );
    }

    #[test]
    fn a_writer_that_numbers_from_zero_is_taken_at_its_word() {
        let table = Materials::from_dot_vox(&[
            material(0, &[("_type", "_emit"), ("_emit", "1")]),
            material(5, &[("_type", "_emit"), ("_emit", "1")]),
        ]);
        assert!(table.get(0).is_emissive(), "id 0 can only mean index 0");
        assert!(table.get(5).is_emissive());
        assert!(!table.get(4).is_emissive());
    }

    #[test]
    fn flux_multiplies_the_emission() {
        let plain = read_material(&material(1, &[("_type", "_emit"), ("_emit", "0.5")]));
        let bright = read_material(&material(
            1,
            &[("_type", "_emit"), ("_emit", "0.5"), ("_flux", "2")],
        ));
        assert_eq!(plain.emit, 0.5);
        assert_eq!(bright.emit, 1.5);
    }

    #[test]
    fn properties_the_type_does_not_use_are_ignored() {
        // Straight out of a real file: an emissive material carrying the
        // metalness left over from when it was something else.
        let m = read_material(&material(
            1,
            &[
                ("_type", "_emit"),
                ("_emit", "1"),
                ("_metal", "0.77"),
                ("_alpha", "0.5"),
            ],
        ));
        assert_eq!(m.metal, 0.0, "an emissive surface is not a metal one");
        assert_eq!(m.opacity, 1.0, "an emissive surface is not glass");
        assert!(m.is_emissive());
    }

    #[test]
    fn glass_alpha_is_transparency_not_coverage() {
        let m = read_material(&material(1, &[("_type", "_glass"), ("_alpha", "0.9")]));
        assert!(
            (m.opacity - 0.1).abs() < 1e-6,
            "alpha 0.9 is nearly clear glass, got opacity {}",
            m.opacity
        );
        assert!(m.is_transparent());
    }

    #[test]
    fn a_diffuse_material_is_indistinguishable_from_none() {
        let table = Materials::from_dot_vox(&[material(
            1,
            &[("_type", "_diffuse"), ("_rough", "0.4"), ("_weight", "1")],
        )]);
        assert_eq!(table.get(0), Material::default());
        assert!(
            !table.from_file,
            "a table of plain diffuse materials is not worth reporting"
        );
    }

    #[test]
    fn nonsense_values_do_not_escape_the_table() {
        let m = read_material(&material(
            1,
            &[
                ("_type", "_emit"),
                ("_emit", "not a number"),
                ("_flux", "1e30"),
            ],
        ));
        assert!(m.emit.is_finite());
        assert_eq!(m.emit, 0.0);

        let m = read_material(&material(
            1,
            &[("_type", "_metal"), ("_metal", "-5"), ("_rough", "12")],
        ));
        assert_eq!(m.metal, 0.0);
        assert_eq!(m.rough, 1.0);
    }

    #[test]
    fn an_id_beyond_the_palette_is_dropped_rather_than_wrapped() {
        let table = Materials::from_dot_vox(&[
            material(9000, &[("_type", "_emit"), ("_emit", "1")]),
            material(1, &[("_type", "_emit"), ("_emit", "1")]),
        ]);
        assert_eq!(table.emissive_count(), 1);
        assert!(table.get(0).is_emissive());
    }

    #[test]
    fn the_transparent_mask_marks_the_right_bits() {
        let table = Materials::from_dot_vox(&[
            material(1, &[("_type", "_glass"), ("_alpha", "0.5")]),
            material(200, &[("_type", "_glass"), ("_alpha", "0.5")]),
        ]);
        let mask = table.transparent_mask();
        assert_eq!(mask[0] & 1, 1, "index 0 should be marked");
        assert_eq!(mask[3] >> (199 - 192) & 1, 1, "index 199 should be marked");
        assert_eq!(mask.iter().map(|w| w.count_ones()).sum::<u32>(), 2);
    }
}
