# Notes

Things in the `.vox` format or in `dot_vox` that did not behave the way the
obvious reading suggests. Recorded so the next person does not have to
rediscover them.

## `dot_vox::Rotation::from_byte` panics on malformed input

`_r` in an `nTRN` keyframe encodes a signed permutation: bits 0–1 name the
non-zero column of row 0, bits 2–3 that of row 1, row 2 takes whichever column
is left, and bits 4–6 are the per-row signs. Only 48 of the 128 meaningful bit
patterns describe a permutation — the rest either repeat a column or name
column 3, which does not exist.

`dot_vox` handles those with an `assert!`, so `Frame::orientation()` aborts the
process on a corrupt file. That is incompatible with "bad files produce an error
in the HUD, never a crash", so `src/scene.rs` decodes `_r` out of the raw
attribute dictionary itself and returns `None` for the invalid patterns, falling
back to identity. `loader::load_bytes` additionally wraps the whole parse in
`catch_unwind` as a second line of defence; it costs nothing on the success
path.

This is the one place the brief's "patch around `dot_vox` and note why" applied.
Everything else about the parser was fine.

## Bit 7 of `_r` is unused

96 of the 256 byte values decode, not 48: the top bit is not part of the
encoding and is ignored. Files in the wild do have it set.

## `_t` is relative to `size / 2` in *integer* division

The natural reading is that a transform places the model's geometric centre.
That is wrong for odd-sized models, and wrong in a way that only shows up on
odd-sized models — which is why it is easy to ship.

MagicaVoxel's pivot is the *cell* at `size / 2` with integer (truncating)
division, not the point at `size / 2.0`. For even sizes the two agree exactly.
For a 3-wide model they differ by half a cell, and centring on the geometric
midpoint puts every voxel half a unit off the world grid — models that look
subtly soft and never quite line up with the ground plane.

`VoxTransform::voxel_to_world` therefore stays entirely in integers:

```rust
self.rotation.apply(v - size / 2) + self.translation
```

`model_matrix`, which moves the meshed geometry, has to reproduce that exactly.
It needs one extra correction the integer form does not: a negated output axis
maps the local box `[v, v+1]` to `[-(v+1), -v]`, so the matrix carries a
`+1` along each axis the rotation flips. Without it, rotated models are one
voxel out along the flipped axes — a bug that is invisible on a symmetric test
cube and obvious on anything else. `scene.rs` has a test that walks all 48
rotations comparing the matrix against the integer placement.

## `dot_vox::Dict` is not a `std` map

It is an `ahash::AHashMap<String, String>`. Building fixtures with
`std::collections::HashMap` does not type-check, and the error message points at
the field rather than the alias.

## `XYZI` indices are 1-based in the file, 0-based after parsing

The format stores palette index + 1 so that 0 can mean empty. `dot_vox` has
already subtracted one by the time voxels reach us, so `Palette::color(voxel.i)`
is a direct lookup and no further adjustment is correct. `VoxelGrid` re-adds its
own `+1` internally purely so that 0 can mean "empty cell" in its storage — that
is unrelated bookkeeping, and getting the two confused maps palette index 255
onto 1.

## Palettes can be short, absent, or both

A file may carry no `RGBA` chunk at all (use MagicaVoxel's default palette, which
is embedded in `src/palette.rs`), or carry fewer than 256 entries. `Palette` is
always exactly 256 entries, padded from the default, so a corrupt index can
never be out of bounds. Truncating instead would have made index lookups
fallible for no benefit.

## Multiple `nSHP` model entries are keyframes

An `nSHP` node can list several models. Those are animation frames, not a group
of models to draw together. Rendering all of them stacks every frame of an
animation on top of itself; the viewer draws the first and ignores the rest.

## Scene graphs can be cyclic

Nothing in the format prevents it, and a corrupt file will do it. `flatten`
carries both a depth limit and a visiting set.

---

## Not a format problem, but worth writing down

* **No MSAA in v1.** Voxel silhouettes are all axis-aligned, so the aliasing is
  mild, and a 4× multisampled target costs real bandwidth on the integrated
  GPUs this targets. It is a one-line change to `MultisampleState` plus a
  resolve target if it turns out to be wanted.

* **The HUD font is embedded.** A 5×7 bitmap atlas in `src/font.rs` rather than
  a font-rasteriser dependency, so there is no system font lookup to fail on a
  bare Linux box and no extra crate in the tree.

* **The file watcher watches the directory, not the file.** MagicaVoxel and most
  exporters save by writing a temporary file and renaming it into place. That
  replaces the inode, and a watch on the file itself is left pointing at the old
  one — the first save works and every save after it is silently missed.

* **wgpu 30 moved several things.** `present` is on `Queue`, not
  `SurfaceTexture`; `get_current_texture` returns a `CurrentSurfaceTexture` enum
  rather than a `Result<_, SurfaceError>`; pipeline layouts take
  `&[Option<&BindGroupLayout>]` and an `immediate_size` in place of push-constant
  ranges; `multiview` is now `multiview_mask`. Older examples on the web do not
  compile against it.

* **The file menu takes the whole keyboard while it is open.** The viewer's
  shortcuts are bare single letters, and a filter box needs those same letters,
  so there is no arrangement in which both work at once. The menu wins while it
  is open, and the HUD keeps showing the toggle states so it is obvious the
  keys have not been lost. Requiring a modifier for the filter would have been
  the alternative; it makes the common case worse to keep the rare one.

* **Menu hit-testing shares its arithmetic with menu layout.** Both go through
  `hud::Metrics`, and a test checks the row tops it reports against the glyph
  positions `hud::layout` actually emits. Two copies of the padding maths drift
  the first time one of them changes, and the symptom -- clicks landing one row
  off -- is easy to misread as an input bug.

* **A pipeline's depth state must match the pass, even when it ignores depth.**
  The HUD draws last and wants no depth testing, but it shares a render pass
  with the model, so it has to declare the same depth format with
  `CompareFunction::Always` and no depth writes. `depth_stencil: None` is a
  validation error there, not a no-op.
